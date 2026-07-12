# Codegen Slice 2C (Enums & the Symbol-tag Value ABI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compile Nymph `enum` declarations and variant construction/reference (bare and qualified) to the Symbol-tag JS ABI so enum values round-trip and carry collision-free variant identity under Node.

**Architecture:** Enums lower to a per-enum object of variant factories/singletons keyed by a shared well-known symbol `TAG = Symbol.for("nymph.tag")`, with each variant carrying a unique unregistered `Symbol(...)` for identity. Because bare variant names (`None`, `Some`) are ambiguous across enums, the **checker records** each variant expression's resolved `(enum, variant)` names in a NodeId-keyed side-table; lowering consumes it (no re-resolution). Codegen emits the ABI and updates the `equality.ts` companion from the old `"~tag"` string scheme to `[TAG]`.

**Tech Stack:** Rust (nightly), oxc 0.139 (`AstBuilder` + `Codegen`), TypeScript (stdlib companion), jj VCS, Node for execution tests.

## Global Constraints

- **Toolchain:** every cargo command uses `cargo +nightly` (the shell pins stable 1.96.0, which fails to build).
- **VCS is jj**, not git. Commit with `jj commit -m "line1" -m "line2" …` (multiple `-m` = paragraphs). NEVER `$(cat <<EOF)` for messages — `cat` is aliased to `bat` and corrupts them with ANSI. Path-scope with trailing path args. Read commits with `jj --no-pager`.
- **oxc is 0.139**; `AstBuilder` construction API is `#[deprecated]` (module-scoped `#![allow(deprecated)]` in `emit.rs`); verify every builder call by compiling.
- **Codegen stays type-free.** Lowering bakes decisions into HIR node shapes; `emit.rs` never consults types/annotations.
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

## Design Decisions (scope)

- **Symbol-tag ABI** (from the design doc §"Value ABI"): one module-level `const TAG = Symbol.for("nymph.tag")` (emitted once, only if the module has any enum). Each enum `E` emits `const E = {}` plus a block scoping one unique `Symbol("E.Variant")` per variant:
  ```js
  const TAG = Symbol.for("nymph.tag");
  const Bound = {};
  {
    const tIncluded = Symbol("Bound.Included");
    Bound.Included = Object.assign(
      (fields) => ({ [TAG]: tIncluded, ...fields }),   // field-variant factory
      { [TAG]: tIncluded },                            // factory also carries its tag
    );
    const tUnbounded = Symbol("Bound.Unbounded");
    Bound.Unbounded = Object.freeze({ [TAG]: tUnbounded }); // nullary singleton
  }
  ```
- **Object-argument factories** (`(fields) => ({ [TAG]: t, ...fields })`), consistent with the struct constructor from Slice 2B — labeled construction args pass as an object, so field order never matters. (This is the illustrative design sketch's positional `(value) => …` adapted to the object convention used repo-wide.)
- **Construction** (`Some(value = 1)`, `Option.Some(value = 1)`) → `E.Variant({ value: 1 })`. Labeled args only (positional deferred, panics — same as 2B structs).
- **Nullary reference** (`None`, `Option.None`) → `E.Variant` (the frozen singleton).
- **Variant detection is checker-driven:** the checker records `(enum_name, variant_name)` per variant expression; lowering reads it. This handles bare/ambiguous names soundly.
- **`equality.ts`** updated from `"~tag"` to `[TAG]` (symbol identity for the discriminant; string-keyed fields still compared structurally, since symbol keys are invisible to `Object.entries`).
- **Deferred:** pattern matching / `is` (Slice 3); defensive `Copy` (no mutation path yet); positional & spread construction args; enum instance methods / namespaced statics (Slice 5). Field access on a variant value already works via Slice 2B's `Field` node.

---

## Task 1: HIR enum + VariantNew/VariantRef nodes

**Files:**
- Modify: `crates/nymph-hir/src/hir.rs`
- Modify: `crates/nymph-codegen/src/emit.rs` (temporary `unreachable!` arms + ignore enums, to keep compiling until Task 4)
- Modify: `crates/nymph-sema/src/lower_hir.rs` (add `enums: Vec::new()` stub at the one `HirModule` construction site)

**Interfaces:**
- Produces: `HirEnum { name: EcoString, variants: Vec<HirVariant> }`; `HirVariant { name: EcoString, fields: Vec<EcoString> }` (nullary ⇔ `fields.is_empty()`); `HirModule.enums: Vec<HirEnum>`; `HirExpr::VariantNew { enum_name: EcoString, variant: EcoString, fields: Vec<(EcoString, HirExpr)> }`; `HirExpr::VariantRef { enum_name: EcoString, variant: EcoString }`.

- [ ] **Step 1: Add the enum type and expr variants**

In `crates/nymph-hir/src/hir.rs`, extend `HirModule` and add the enum types:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct HirModule {
	pub funcs: Vec<HirFunc>,
	pub classes: Vec<HirClass>,
	pub enums: Vec<HirEnum>,
}

/// An `enum` declaration → the Symbol-tag ABI object. Each variant becomes a
/// factory (fields) or a frozen singleton (nullary).
#[derive(Clone, Debug, PartialEq)]
pub struct HirEnum {
	pub name: EcoString,
	pub variants: Vec<HirVariant>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirVariant {
	pub name: EcoString,
	/// Field names in declaration order; empty ⇒ nullary singleton variant.
	pub fields: Vec<EcoString>,
}
```

Add to `enum HirExpr` (near `New`/`Field`):

```rust
	/// Variant construction → `<enum>.<variant>({ field: value, … })`.
	VariantNew {
		enum_name: EcoString,
		variant: EcoString,
		fields: Vec<(EcoString, HirExpr)>,
	},
	/// Nullary variant reference → `<enum>.<variant>` (the frozen singleton).
	VariantRef {
		enum_name: EcoString,
		variant: EcoString,
	},
```

- [ ] **Step 2: Keep `emit.rs` compiling and stub the lowering site**

In `emit.rs::emit_expr`, add a temporary arm near the `New`/`Field` arm:

```rust
			HirExpr::VariantNew { .. } | HirExpr::VariantRef { .. } => {
				unreachable!("emitted in Task 4")
			}
```

`emit_module` ignores `module.enums` for now. In `lower_hir.rs::lower_module`, add `enums: Vec::new()` to the `HirModule { … }` literal so the tree builds.

- [ ] **Step 3: Build**

Run: `cargo +nightly build -p nymph-codegen`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
cargo +nightly fmt -p nymph-hir -p nymph-codegen -p nymph-sema
jj commit -m "feat(hir): enum decls + VariantNew/VariantRef expr nodes" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-hir crates/nymph-codegen crates/nymph-sema
```

---

## Task 2: Checker records variant resolution

**Files:**
- Modify: `crates/nymph-sema/src/annotate.rs`
- Modify: `crates/nymph-sema/src/infer_expr.rs`
- Test: `crates/nymph-sema/tests/annotate.rs`

**Interfaces:**
- Produces: `Annotations::variant_of(id) -> Option<&VariantResolution>` where `VariantResolution { enum_name: EcoString, variant: EcoString }`; recorded on each variant construction/reference node.
- Consumes: existing `variant_value`, `infer_variant_ctor`, `resolve_variant`, `DefData.name`, `VariantSig.name`.

- [ ] **Step 1: Add the side-table**

In `crates/nymph-sema/src/annotate.rs`, refactor the tuple-struct `Annotations` into a named struct with a second map (keeps `ExprInfo` `Copy`):

```rust
/// The resolved `(enum, variant)` names behind a variant construction or
/// reference, recorded so lowering can emit the Symbol-tag ABI without
/// re-resolving ambiguous bare variant names.
#[derive(Clone, Debug)]
pub struct VariantResolution {
	pub enum_name: ecow::EcoString,
	pub variant: ecow::EcoString,
}

#[derive(Clone, Debug, Default)]
pub struct Annotations {
	infos: FxHashMap<NodeId, ExprInfo>,
	variants: FxHashMap<NodeId, VariantResolution>,
}
```

Update every method that used `self.0` to use `self.infos` (`get`, `len`, `is_empty`, `record`, `record_resolution`). Add:

```rust
	/// Record which `(enum, variant)` a variant expression resolved to.
	pub(crate) fn record_variant(&mut self, id: NodeId, res: VariantResolution) {
		if id != NodeId::DUMMY {
			self.variants.insert(id, res);
		}
	}

	pub fn variant_of(&self, id: NodeId) -> Option<&VariantResolution> {
		self.variants.get(&id)
	}
```

- [ ] **Step 2: Write the failing recording test**

Add to `crates/nymph-sema/tests/annotate.rs` (uses the existing `parse` helper):

```rust
#[test]
fn records_variant_resolution() {
	let module = parse(
		r#"
		enum Opt(Some(value: int), None)
		func f(): Opt = Some(value = 1)
		func g(): Opt = None
		func h(): Opt = Opt.Some(value = 2)
		"#,
	);
	let checked = check_module(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);

	// Walk every func body, asserting each variant expr carries a resolution.
	let mut found = 0;
	for member in &module.members {
		let nymph_ast::decl::Declaration::Func { body, .. } = member else {
			continue;
		};
		let mut ids = Vec::new();
		collect_expr_ids(body, &mut ids);
		for id in ids {
			if let Some(res) = checked.annotations.variant_of(id) {
				assert_eq!(res.enum_name, "Opt");
				assert!(res.variant == "Some" || res.variant == "None");
				found += 1;
			}
		}
	}
	assert_eq!(found, 3, "Some, None, Opt.Some each recorded once");
}
```

> Verify the enum-declaration syntax against `crates/nymph-sema/tests/check.rs` (enums are written `enum Name(Variant(field: T), Nullary)` or with a `{ … }` body). Adjust the source if the parenthesized-variant form differs. Extend `collect_expr_ids` (already in this file) if needed so it recurses through `Call`, `MemberAccess`, and `CallArg` values to reach the variant nodes.

- [ ] **Step 3: Run; expect failure**

Run: `cargo +nightly test -p nymph-sema --test annotate records_variant_resolution`
Expected: FAIL — `variant_of` returns `None` (recording not wired yet).

- [ ] **Step 4: Record at the two resolution sinks**

In `crates/nymph-sema/src/infer_expr.rs`, thread the resolving expression's `NodeId` into the two sink methods and record there. Both sinks can build the resolution from `enum_def` + `variant`:

```rust
	fn variant_resolution(&self, enum_def: DefId, variant: usize) -> crate::annotate::VariantResolution {
		crate::annotate::VariantResolution {
			enum_name: self.defs.data(enum_def).name.clone(),
			variant: self.sigs.enums[&enum_def].variants[variant].name.clone(),
		}
	}
```

- Give `variant_value` and `infer_variant_ctor` an extra `id: NodeId` parameter; at the top of each, `let res = self.variant_resolution(enum_def, variant); self.annotations.record_variant(id, res);`.
- Thread `expr.id` from `infer_kind` into the callers: `infer_identifier(&name.0, span, expr.id)`, `infer_call(func, args, span, expr.id)`, `infer_member(parent, &member.0, member.1, expr.id)`. Propagate the `id` into their internal `variant_value` / `infer_variant_ctor` / `type_of_def` calls. `type_of_def` also needs the `id` to forward to `variant_value` for its `DefKind::Variant` arm.

> There are exactly five call paths that reach a variant (bare ref via `infer_identifier`→`resolve_variant`; def-table variant via `type_of_def`; bare ctor and qualified ctor in `infer_call`; qualified nullary ref in `infer_member`). All funnel through `variant_value` or `infer_variant_ctor`, so recording in those two methods covers them once each `id` is threaded. Let the compiler find every call site you must update.

- [ ] **Step 5: Run recording test + full sema suite**

Run: `cargo +nightly test -p nymph-sema`
Expected: PASS — the new recording test plus all existing sema tests.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo +nightly fmt -p nymph-sema && cargo +nightly clippy -p nymph-sema --all-targets
jj commit -m "feat(sema): record (enum, variant) resolution for variant exprs" -m "Threads the node id into variant_value/infer_variant_ctor and records the resolved enum+variant names in a NodeId-keyed side-table, so lowering can emit the Symbol-tag ABI without re-resolving ambiguous bare variant names." -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-sema
```

---

## Task 3: Lower enum decls and variant exprs

**Files:**
- Modify: `crates/nymph-sema/src/lower_hir.rs`
- Test: `crates/nymph-sema/tests/lower_hir.rs`

**Interfaces:**
- Consumes: `HirEnum`/`HirVariant`, `HirExpr::{VariantNew, VariantRef}` (Task 1); `Annotations::variant_of` (Task 2); `Declaration::Enum { name, variants, .. }`, `EnumVariant { name, fields }`.

- [ ] **Step 1: Write the failing lowering tests**

Add to `crates/nymph-sema/tests/lower_hir.rs`:

```rust
#[test]
fn lowers_enum_decl_and_variants() {
	let hir = lower(
		r#"
		enum Opt(Some(value: int), None)
		func s(): Opt = Some(value = 1)
		func n(): Opt = None
		func q(): Opt = Opt.Some(value = 2)
		"#,
	);
	// The enum becomes an HirEnum with both variants (Some carries a field, None nullary).
	assert_eq!(hir.enums.len(), 1);
	assert_eq!(hir.enums[0].name, "Opt");
	let some = hir.enums[0].variants.iter().find(|v| v.name == "Some").unwrap();
	let none = hir.enums[0].variants.iter().find(|v| v.name == "None").unwrap();
	assert_eq!(some.fields, vec!["value".to_string()]);
	assert!(none.fields.is_empty());

	// Bare construction, bare nullary ref, and qualified construction all lower.
	let body = |name: &str| hir.funcs.iter().find(|f| f.name == name).unwrap().body.clone();
	assert!(matches!(body("s"), HirExpr::VariantNew { .. }), "bare ctor → VariantNew");
	assert!(matches!(body("n"), HirExpr::VariantRef { .. }), "bare nullary → VariantRef");
	assert!(matches!(body("q"), HirExpr::VariantNew { .. }), "qualified ctor → VariantNew");
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-sema --test lower_hir lowers_enum_decl_and_variants`
Expected: FAIL — `lower_module` ignores enums and the variant exprs lower as plain call/identifier/member.

- [ ] **Step 3: Lower enum declarations**

In `lower_hir.rs::lower_module`, add an `enums` accumulator and an arm:

```rust
				Declaration::Enum { name, variants, .. } => enums.push(HirEnum {
					name: name.0.clone(),
					variants: variants
						.iter()
						.map(|v| HirVariant {
							name: v.0.name.0.clone(),
							fields: v.0.fields.iter().map(|f| f.0.name.0.clone()).collect(),
						})
						.collect(),
				}),
```

Return `HirModule { funcs, classes, enums }`. Add `HirEnum, HirVariant` to the `nymph_hir::hir` import.

- [ ] **Step 4: Lower variant construction and reference**

Variant detection is by annotation. In `lower_expr`, before the existing `Call` struct-construction check, handle a recorded variant; and in the `Identifier`/`MemberAccess` arms, emit `VariantRef` when the node carries a resolution. Add a small helper:

```rust
	fn variant_new(&self, id: nymph_ast::NodeId, args: &[nymph_ast::Spanned<nymph_ast::expr::CallArg>]) -> Option<HirExpr> {
		let res = self.annotations.variant_of(id)?;
		let fields = args
			.iter()
			.map(|a| {
				let label = a.0.name.as_ref().unwrap_or_else(|| {
					panic!("slice-2c variant construction requires labeled fields")
				});
				(label.0.clone(), self.lower_expr(&a.0.value))
			})
			.collect();
		Some(HirExpr::VariantNew {
			enum_name: res.enum_name.clone(),
			variant: res.variant.clone(),
			fields,
		})
	}
```

- In the `ExprKind::Call { func, args, .. }` arm, first: `if let Some(v) = self.variant_new(expr.id, args) { v } else if <struct-name check> { … New … } else { … Call … }`.
- In `ExprKind::Identifier(name)`: `if let Some(res) = self.annotations.variant_of(expr.id) { HirExpr::VariantRef { enum_name: res.enum_name.clone(), variant: res.variant.clone() } } else { HirExpr::Local(name.0.clone()) }`.
- In `ExprKind::MemberAccess { parent, member, .. }`: `if let Some(res) = self.annotations.variant_of(expr.id) { HirExpr::VariantRef { … } } else { HirExpr::Field { … } }`.

> A qualified nullary ref `Opt.None` records the resolution on the `MemberAccess` node (Task 2), so it lowers to `VariantRef`, not `Field`. A qualified construction `Opt.Some(value=1)` is a `Call` whose *call node* carries the resolution — handled by `variant_new(expr.id, args)`, so the inner `MemberAccess` callee is never lowered. Good.

- [ ] **Step 5: Run lowering tests + sema suite**

Run: `cargo +nightly test -p nymph-sema`
Expected: PASS — new enum lowering test plus all existing.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo +nightly fmt -p nymph-sema && cargo +nightly clippy -p nymph-sema --all-targets
jj commit -m "feat(sema): lower enum decls to HirEnum and variant exprs to VariantNew/VariantRef" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-sema
```

---

## Task 4: Emit the Symbol-tag ABI + update equality.ts, run under Node

**Files:**
- Modify: `crates/nymph-codegen/src/emit.rs`
- Modify: `stdlib/src/ops/equality.ts`
- Test: `crates/nymph-codegen/tests/run_node.rs`

**Interfaces:**
- Consumes: `HirModule.enums`, `HirEnum`/`HirVariant`, `HirExpr::{VariantNew, VariantRef}`.

- [ ] **Step 1: Write the failing Node-execution tests**

Add to `crates/nymph-codegen/tests/run_node.rs`:

```rust
#[test]
fn runs_enum_field_variant() {
	// A field variant constructs via its factory; a field reads back.
	let src = r#"
		enum Opt(Some(value: int), None)
		func mk(): Opt = Some(value = 7)
	"#;
	assert_eq!(run(src, "mk().value"), "7");
}

#[test]
fn runs_enum_nullary_identity() {
	// A nullary variant is a singleton: two references are identical.
	let src = r#"
		enum Opt(Some(value: int), None)
		func none(): Opt = None
	"#;
	assert_eq!(run(src, "none() === Opt.None"), "true");
}

#[test]
fn runs_enum_variant_tag_distinct() {
	// Same-named variants in different enums have distinct tags. The shared TAG
	// symbol is reachable; two different variants are not tag-equal.
	let src = r#"
		enum A(X(n: int), Y)
		func ax(): A = A.X(n = 1)
	"#;
	let tag = "Symbol.for('nymph.tag')";
	assert_eq!(run(src, &format!("ax()[{tag}] === A.X[{tag}]")), "true");
	assert_eq!(run(src, &format!("ax()[{tag}] === A.Y[{tag}]")), "false");
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-codegen --test run_node runs_enum_field_variant`
Expected: FAIL — no enum object emitted; the variant nodes hit the `unreachable!` from Task 1.

- [ ] **Step 3: Emit the TAG const and enum blocks**

In `emit.rs::emit_module`, before classes/functions, if `!module.enums.is_empty()` emit `const TAG = Symbol.for("nymph.tag");` once, then one `emit_enum` per enum. Add `emit_enum` producing the shape in Design Decisions. Structure (verify every oxc 0.139 builder by compiling — model on existing helpers, and reuse `expression_object`/`member_expression_static`/`expression_call`/`alloc_function` already in `emit.rs`):

- `const <E> = {};`
- a block statement `{ … }` containing, per variant:
  - `const t<Variant> = Symbol("<E>.<Variant>");` — a call to `Symbol` with a string-literal arg.
  - field variant: `<E>.<Variant> = Object.assign((fields) => ({ [TAG]: t<Variant>, ...fields }), { [TAG]: t<Variant> });`
    - the arrow returns a **parenthesized object** with a computed `[TAG]` property, a spread element `...fields`, then `Object.assign(arrow, tagObj)` where `tagObj` is `{ [TAG]: t<Variant> }`.
  - nullary variant: `<E>.<Variant> = Object.freeze({ [TAG]: t<Variant> });`
- The computed `[TAG]` object property uses `object_property(SPAN, PropertyKind::Init, <computed key TAG ident>, <value>, false, false, /*computed*/ true)`. The spread `...fields` is an `ObjectPropertyKind::SpreadProperty`. Verify these builder names (`spread_element`/`object_property`/`expression_arrow_function`) against oxc 0.139.

> This is the fiddliest emission in the codebase so far. Build incrementally: get `const E = {}` + an empty block compiling first, then add the nullary singleton, then the field-variant factory. Print and eyeball the JS via a scratch test if needed. If the computed-key or spread builders prove awkward, an acceptable equivalent factory body is `(fields) => Object.assign({ [TAG]: t<Variant> }, fields)` (an `Object.assign` call instead of a spread literal) — same runtime result, simpler oxc.

- [ ] **Step 4: Emit VariantNew and VariantRef**

Replace the temporary arm from Task 1:

```rust
			// <enum>.<variant>({ field: value, … })
			HirExpr::VariantNew { enum_name, variant, fields } => {
				// object arg { field: value, … } — same shape as struct `New`
				let mut props = self.ast.vec();
				for (name, value) in fields {
					let key = self.ast.property_key_static_identifier(SPAN, self.ast.allocator.alloc_str(name));
					let val = self.emit_expr(value);
					props.push(ObjectPropertyKind::ObjectProperty(self.ast.alloc_object_property(
						SPAN, PropertyKind::Init, key, val, false, false, false,
					)));
				}
				let obj = self.ast.expression_object(SPAN, props);
				// callee: <enum>.<variant>
				let callee = Expression::from(self.ast.member_expression_static(
					SPAN,
					self.ast.expression_identifier(SPAN, self.ast.allocator.alloc_str(enum_name)),
					self.ast.identifier_name(SPAN, self.ast.allocator.alloc_str(variant)),
					false,
				));
				let mut args = self.ast.vec();
				args.push(Argument::from(obj));
				self.ast.expression_call(SPAN, callee, oxc::ast::NONE, args, false)
			}
			// <enum>.<variant>
			HirExpr::VariantRef { enum_name, variant } => Expression::from(self.ast.member_expression_static(
				SPAN,
				self.ast.expression_identifier(SPAN, self.ast.allocator.alloc_str(enum_name)),
				self.ast.identifier_name(SPAN, self.ast.allocator.alloc_str(variant)),
				false,
			)),
```

- [ ] **Step 5: Update `equality.ts` to the `[TAG]` scheme**

Rewrite the ADT branch of `stdlib/src/ops/equality.ts` to compare by symbol tag. Add the shared symbol at the top and replace the `"~tag"` block:

```ts
const TAG = Symbol.for("nymph.tag");

export function equals<T>($_this: T, other: T): boolean {
	// … unchanged primitive / array / Map branches …

	if (
		typeof $_this === "object" &&
		typeof other === "object" &&
		$_this &&
		other &&
		(TAG in $_this) &&
		(TAG in other) &&
		($_this as any)[TAG] === (other as any)[TAG] &&               // variant identity
		Object.keys($_this).length === Object.keys(other).length &&   // symbol keys excluded
		Object.entries($_this).every(([k, v]) => equals(v, (other as any)[k]))
	) {
		return true;
	}

	return false;
}
```

> `Object.keys`/`Object.entries` do not enumerate symbol keys, so `TAG` is excluded from the structural field comparison automatically — only the variant's own string-keyed fields are compared. Confirm the file compiles with the repo's oxc/TS tooling (`pnpm --filter nymph-docs`… is unrelated; equality.ts is stdlib TS — a `pnpm lint`/`oxlint` pass at the root covers it). Keep the primitive/array/Map branches byte-for-byte.

- [ ] **Step 6: Run execution tests**

Run: `cargo +nightly test -p nymph-codegen --test run_node`
Expected: PASS — `mk().value`→7, `none() === Opt.None`→true, tag identity true/false, plus all Slice 1/2A/2B tests.

- [ ] **Step 7: Full workspace gate + fmt + commit**

Run: `cargo +nightly test && cargo +nightly clippy --all-targets`
Expected: green/clean (ignore pre-existing warnings in crates you don't own).

```bash
cargo +nightly fmt -p nymph-codegen
jj commit -m "feat(codegen): emit the Symbol-tag enum ABI + variant construction/reference; update equality.ts" -m "Enums emit as a TAG=Symbol.for(nymph.tag) discriminant plus per-enum objects of variant factories (fields) / frozen singletons (nullary), each carrying a unique Symbol identity. VariantNew → E.V({…}); VariantRef → E.V. equality.ts moves from the ~tag string to [TAG] symbol identity. Node: mk().value→7, none()===Opt.None, cross-enum tags distinct." -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-codegen stdlib/src/ops/equality.ts
```

---

## Self-Review

**Spec coverage (design §"Value ABI (the .ts contract, with Symbol tags)"):**
- `TAG = Symbol.for("nymph.tag")` shared discriminant ✓
- Per-variant unique `Symbol(...)`; field factory + `[TAG]`; nullary frozen singleton ✓
- Construction (bare + qualified) ✓; nullary reference (bare + qualified) ✓; field access on a variant value ✓ (reuses 2B `Field`)
- `equality.ts` `~tag` → `[TAG]` ✓
- Cross-enum same-named variants get distinct tags ✓ (unregistered symbols)

**Deferred, correctly:** pattern matching / `is` (Slice 3 — the `x?.[TAG] === E.V[TAG]` identity test compiles there); defensive `Copy` (needs a mutation path — no index/field assignment yet); positional & spread construction args; enum methods / namespaced statics (Slice 5).

**Placeholder scan:** no "TBD"/"handle edge cases". oxc 0.139 builders for the ABI (computed key, spread, arrow, `Symbol`/`Object.freeze`/`Object.assign` calls) are given at reference shapes with a compile-to-verify instruction and a documented simpler fallback (`Object.assign` factory body). Enum/variant AST projections carry "verify against current code" notes.

**Type consistency:** `HirModule { funcs, classes, enums }`; `HirEnum { name, variants: Vec<HirVariant> }`; `HirVariant { name, fields }`; `HirExpr::VariantNew { enum_name, variant, fields }` / `VariantRef { enum_name, variant }`; `Annotations::variant_of(NodeId) -> Option<&VariantResolution>`; `VariantResolution { enum_name, variant }`. Names/shapes consistent across Tasks 1–4.

**Scope:** one coherent, Node-testable increment — the enum value form and its collision-free identity ABI, plus the companion update the ABI requires. Matching and Copy are separate follow-ons.
