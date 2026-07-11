# Nymph Codegen — Slice 2A (Collections & Interner Threading) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Emit JavaScript for Nymph's collection value forms — tuples, lists, and maps — plus built-in index access (`list[i]`, `map[k]`), and stand up the interner-threading foundation that lets the lowering pass make type-directed codegen decisions.

**Architecture:** This is the first part of Slice 2 (Data types & value ABI) from `docs/superpowers/specs/2026-07-05-nymph-codegen-design.md`. It introduces the architectural shift Slice 2 needs: the checker's `Interner` now travels in the `Checked` result, and `lower_hir` consumes it (plus the per-node annotations) so lowering can resolve type-dependent choices into concrete, **type-free** HIR nodes — keeping codegen a dumb printer, exactly as in Slice 1. The first genuine consumer is index access, which lowering dispatches to a JS subscript (`arr[i]`) for lists/tuples or a method call (`map.get(k)`) for maps by asking `Interner::kind` about the receiver's recorded type.

**Tech Stack:** Rust (edition 2024, nightly), `oxc` 0.138, the existing crates. Node.js (v26, `node` on PATH) for execution tests.

## Global Constraints

- **Toolchain:** every cargo command MUST be prefixed `cargo +nightly` (the shell pins stable, which breaks the build otherwise).
- **oxc is 0.138**; the `AstBuilder` construction API is `#[deprecated]` there with no shipped replacement — `crates/nymph-codegen/src/emit.rs` already carries a module-scoped `#![allow(deprecated)]`; keep using it. Verify every new `ast.*` builder against 0.138 by compiling (names/args shift between oxc minors); the reference emitter (`reference/compiler/src/transpiler/emit.rs`, oxc 0.123) is a close guide, not gospel.
- **Value ABI (locked by the spec, honored here):** tuples and lists → JS arrays; maps → JS `Map`. (Structs → classes and enums → `{ [TAG]: sym, … }` land in Slice 2B/2C, not here.)
- **HIR stays type-free.** Lowering resolves every type-dependent decision into a concrete HIR node shape; codegen never consults a `Ty` or the `Interner`. (Continuation of the Slice 1 YAGNI decision.)
- **Formatting/lints:** finish each task `cargo +nightly fmt` clean and `cargo +nightly clippy --all-targets` clean for the crates you touched. Scope `cargo fmt`/commits to your own crates — the error-code crates (`nymph-diagnostics`, `nymph-errorcode`, the `errors.rs` files) are owned elsewhere; do not reformat or commit them.
- **VCS is Jujutsu (jj).** In subagent execution the controller owns commits (`jj commit <paths>`); implementers do not run git/jj and skip the plan's "Commit" steps.
- **No behavior change to type checking's diagnostics** beyond the deliberate new index fast-path (Task 2). Existing sema tests are the regression harness.

## Design Decisions (locked)

1. **`Interner` travels in `Checked`.** `check_module`/`check_program` move the checker's interner into `Checked { diags, annotations, interner }` when checking finishes (it is otherwise dropped). This is the fix the Slice 0 whole-branch review deferred to "the first slice that interprets a `Ty`" — that is now.
2. **Uniform type recording.** The checker records the resolved type of *every* expression it infers (a thin wrapper around `infer`), so lowering can look up any node's type. Operator/method `Resolution` recording (still deferred to Slice 4) is kept separate via a `record_resolution` method so the two never clobber each other.
3. **Index access is built-in in Slice 2A.** The checker gains a fast-path: indexing a `List` yields its element type and indexing a `Map` yields its value type, with **no `Index` interface impl required in scope** (so test snippets need no prelude). User `Index` overloads on ADTs remain a Slice 4 concern (the existing `resolve_method("index", …)` path is the fallback). Lowering dispatches on the receiver's recorded type: `List`/`Tuple` → a JS subscript node, `Map` → a map-get node.
4. **Tuples and lists share one HIR node** (`Array`), since both emit as JS arrays. Spreads (`#[a, ...rest]`, `#(...other)`) are **out of scope for 2A** (items only); lowering `panic!`s on a spread element, as other unimplemented forms already do.

---

## File Structure

- `crates/nymph-hir/src/hir.rs` — add `HirExpr::{Array, MapLit, Index, MapGet}`.
- `crates/nymph-sema/src/annotate.rs` — `Checked` gains `interner`; `Annotations` gains `record_resolution`.
- `crates/nymph-sema/src/check.rs` — move the interner into `Checked`; a `record` wrapper already exists.
- `crates/nymph-sema/src/infer_expr.rs` — uniform type recording in `infer`; built-in index fast-path.
- `crates/nymph-sema/src/lower_hir.rs` — `lower_hir(&Module, &Checked)`; lower tuple/list/map literals + index access.
- `crates/nymph-codegen/src/emit.rs` — emit `Array`/`MapLit`/`Index`/`MapGet`.
- `crates/nymph-codegen/src/lib.rs` — thread the interner through `compile`.
- `crates/nymph-codegen/tests/run_node.rs` + `crates/nymph-sema/tests/lower_hir.rs` — tests.

---

## Task 1: `Checked` carries the `Interner`

**Files:**
- Modify: `crates/nymph-sema/src/annotate.rs`, `crates/nymph-sema/src/check.rs`
- Modify: `crates/nymph-codegen/src/lib.rs` (the `emit`/`compile` call sites), `crates/nymph-sema/src/lower_hir.rs` (signature — full lowering change is Task 3; here just keep it compiling)
- Test: existing sema + codegen suites are the gate.

**Interfaces:**
- Produces: `Checked { pub diags: Vec<Diagnostic>, pub annotations: Annotations, pub interner: nymph_hir::ty::Interner }`.

- [ ] **Step 1: Add the field**

In `crates/nymph-sema/src/annotate.rs`, add to `Checked`:

```rust
use crate::ty::Interner;

#[derive(Clone, Debug)]
pub struct Checked {
    pub diags: Vec<Diagnostic>,
    pub annotations: Annotations,
    /// The interner that minted the types in `annotations`. A `Ty` is meaningless
    /// without it, so it travels with the result for the lowering pass to consult.
    pub interner: Interner,
}
```

(Confirm `Interner` derives `Clone, Debug`; if not, either add the derives in `crates/nymph-hir/src/ty/mod.rs` or move rather than clone it — see Step 2.)

- [ ] **Step 2: Move the interner out of the checker at the end of checking**

In `crates/nymph-sema/src/check.rs`, in both `check_module` and `check_program` (which delegates to `check_module`), build the result by moving the interner out:

```rust
    Checked {
        diags: checker.diags,
        annotations: checker.annotations,
        interner: checker.interner,
    }
```

If the borrow checker complains about partial moves out of `checker`, destructure the fields into locals before constructing `Checked`, or take `std::mem::take(&mut checker.interner)` (add `Default` to `Interner` if needed). Prefer a clean move.

- [ ] **Step 3: Keep call sites compiling**

`nymph_codegen::compile` and the `run_node.rs` test helper call `check_module(...).diags` / build from the result — they ignore the new field, so they still compile. `lower_hir` still has its Slice-1 signature `lower_hir(&Module)` for now (Task 3 changes it). Run: `cargo +nightly build`.
Expected: PASS.

- [ ] **Step 4: Run the suites**

Run: `cargo +nightly test -p nymph-sema -p nymph-codegen`
Expected: PASS — no behavior change; the field is additive.

- [ ] **Step 5: Format, clippy, commit**

```bash
cargo +nightly fmt -p nymph-sema -p nymph-codegen
cargo +nightly clippy -p nymph-sema -p nymph-codegen --all-targets
git add crates/nymph-sema crates/nymph-codegen
git commit -m "feat(sema): Checked carries the Interner for the lowering pass"
```

---

## Task 2: Uniform type recording + built-in index fast-path

**Files:**
- Modify: `crates/nymph-sema/src/annotate.rs` (`record_resolution`), `crates/nymph-sema/src/infer_expr.rs`
- Test: `crates/nymph-sema/tests/lower_hir.rs` or `annotate.rs` (a recording assertion), plus existing sema suite.

**Interfaces:**
- Consumes: `Checker::record`, `Annotations::record`.
- Produces: `Annotations::record_resolution(&mut self, id: NodeId, resolution: Resolution)` (updates only the resolution of an existing entry, inserting a bare entry if none exists); every `infer`'d expression node has an `ExprInfo` with its resolved `ty`.

- [ ] **Step 1: Write the failing recording test**

Add to `crates/nymph-sema/tests/lower_hir.rs` (or a new `annotate_types.rs`), using the crate's existing parse+check helpers:

```rust
#[test]
fn records_type_of_collection_literals() {
    // A list literal's node should carry a recorded type after checking.
    let module = parse("func f(): #[int] = #[1, 2, 3]");
    let checked = nymph_sema::check_module(&module);
    assert!(checked.diags.is_empty(), "{:?}", checked.diags);
    // Walk to the list-literal node and assert it is annotated. (Reuse a small
    // NodeId walker; the body expr is the `#[1, 2, 3]` list.)
    let ids = collect_expr_ids(&module); // helper as in existing tests
    let annotated = ids.iter().filter(|id| checked.annotations.get(**id).is_some()).count();
    assert_eq!(annotated, ids.len(), "every inferred expression node should be annotated");
}
```

- [ ] **Step 2: Run it; expect failure**

Run: `cargo +nightly test -p nymph-sema records_type_of_collection_literals`
Expected: FAIL — only int-literal and binary-op nodes are recorded today, so the list/element nodes are unannotated.

- [ ] **Step 3: Make `infer` record every node's type uniformly**

In `crates/nymph-sema/src/infer_expr.rs`, restructure `infer` so the big `match` becomes a private `infer_kind`, and `infer` records the resolved type of every node:

```rust
pub(crate) fn infer(&mut self, expr: &Expr) -> Ty {
    let ty = self.infer_kind(expr);
    // Record the node's resolved type for the lowering pass. Zonking happens inside
    // `record`. Returns the *raw* ty so callers can still unify against it.
    self.record(expr.id, ty, None);
    ty
}

fn infer_kind(&mut self, expr: &Expr) -> Ty {
    let span = expr.span;
    match &expr.kind {
        // … the existing arms, MINUS the per-arm `self.record(...)` calls added in
        // Slice 0 for Int and BinaryOp (now redundant — the wrapper records them).
        …
    }
}
```

Remove the explicit `self.record(expr.id, ty, None)` calls previously in the `Int` and `BinaryOp` arms (the wrapper now covers them). Keep the `BinaryOp` `TODO(codegen-slice-4)` comment about its deferred `Resolution`.

> Note the check-mode paths: `check` handles some nodes (widened int literals, value-position `if`/`block`/`match`) without calling `infer`. Those remain unrecorded for now — Slice 2A only needs types for nodes that flow through `infer` (collection literals and index receivers do). Do **not** add a uniform recorder to `check` in this task.

- [ ] **Step 4: Add `record_resolution` (future-proofing operator dispatch)**

In `crates/nymph-sema/src/annotate.rs`:

```rust
impl Annotations {
    /// Attach a `Resolution` to a node, preserving any already-recorded type.
    /// Used by later slices for operator/method dispatch without clobbering the
    /// type recorded by the uniform `infer` wrapper.
    pub(crate) fn record_resolution(&mut self, id: NodeId, resolution: Resolution) {
        if id == NodeId::DUMMY {
            return;
        }
        self.0
            .entry(id)
            .and_modify(|info| info.resolution = Some(resolution))
            .or_insert(ExprInfo { ty: /* placeholder */, resolution: Some(resolution) });
    }
}
```

> Problem: `or_insert` needs a `ty`, but a resolution-only record has none. Resolve by making the `or_insert` branch unreachable in practice (a resolved operator node is always also `infer`'d, so its type is already recorded) — use `and_modify` plus a debug assert, or store `ty: Option<Ty>` if cleaner. Pick one and note it. The simplest correct form: only `and_modify`, and if the entry is absent, that is a bug (assert in debug). Since Slice 2A does not yet *call* `record_resolution`, implement it as `and_modify(...)` with a `debug_assert!` on the missing-entry case and no `or_insert`.

- [ ] **Step 5: Add the built-in index fast-path in the checker**

In `crates/nymph-sema/src/infer_expr.rs`, in the `ExprKind::IndexAccess` arm, before falling back to `resolve_method("index", …)`, fast-path the built-in collections:

```rust
ExprKind::IndexAccess { parent, index, .. } => {
    let recv = self.infer(parent);
    let key = self.infer(index);
    let recv_r = self.shallow_resolve(recv);
    match self.interner.kind(recv_r).clone() {
        TyKind::List(elem) => {
            let int = self.interner.int();
            self.unify(key, int, span); // list index is an int
            elem
        }
        TyKind::Tuple(elems) => {
            // A tuple index yields a fresh var (heterogeneous; precise typing needs a
            // const index and is deferred). Best-effort: the join isn't modelled yet.
            let _ = elems;
            self.fresh()
        }
        TyKind::Map(k, v) => {
            self.unify(key, k, span);
            v
        }
        _ => {
            let key_lit = matches!(index.kind, ExprKind::Int(_));
            match self.resolve_method(recv, "index", &[key], &[key_lit], span) {
                Some(ret) => ret,
                None => self.fresh(),
            }
        }
    }
}
```

> Verify `shallow_resolve`, `unify`, `interner.kind`, and the `TyKind` variant names against the current code. This makes `#[1,2,3][1]` and `#{"a":1}["a"]` type without an `Index` impl in scope.

- [ ] **Step 6: Run the recording test + the full sema suite**

Run: `cargo +nightly test -p nymph-sema`
Expected: PASS — the new recording test passes; all existing sema tests still pass (uniform recording and the index fast-path do not change any diagnostic; index on lists/maps that previously best-efforted now types precisely, which only *removes* potential errors).

- [ ] **Step 7: Format, clippy, commit**

```bash
cargo +nightly fmt -p nymph-sema
cargo +nightly clippy -p nymph-sema --all-targets
git add crates/nymph-sema
git commit -m "feat(sema): record every inferred node's type; built-in list/map index fast-path"
```

---

## Task 3: HIR collection nodes + typed lowering

**Files:**
- Modify: `crates/nymph-hir/src/hir.rs`, `crates/nymph-sema/src/lower_hir.rs`, `crates/nymph-codegen/src/lib.rs` (pass the interner to lowering)
- Test: `crates/nymph-sema/tests/lower_hir.rs`

**Interfaces:**
- Produces (HIR):
  - `HirExpr::Array(Vec<HirExpr>)` — a tuple or list literal.
  - `HirExpr::MapLit(Vec<(HirExpr, HirExpr)>)` — a map literal (key/value pairs).
  - `HirExpr::Index { recv: Box<HirExpr>, index: Box<HirExpr> }` — a JS subscript (list/tuple).
  - `HirExpr::MapGet { recv: Box<HirExpr>, key: Box<HirExpr> }` — `recv.get(key)`.
- Produces (lowering): `lower_hir(module: &Module, checked: &Checked) -> HirModule`.

- [ ] **Step 1: Add the HIR nodes**

In `crates/nymph-hir/src/hir.rs`, add to `HirExpr` (after `Call`):

```rust
    /// A tuple or list literal — both emit as a JS array.
    Array(Vec<HirExpr>),
    /// A map literal — emits as `new Map([[k, v], …])`.
    MapLit(Vec<(HirExpr, HirExpr)>),
    /// A subscript into a list/tuple — emits as `recv[index]`.
    Index {
        recv: Box<HirExpr>,
        index: Box<HirExpr>,
    },
    /// A map lookup — emits as `recv.get(key)`.
    MapGet {
        recv: Box<HirExpr>,
        key: Box<HirExpr>,
    },
```

- [ ] **Step 2: Write the failing lowering tests**

Add to `crates/nymph-sema/tests/lower_hir.rs` (the `lower` helper must now pass a `Checked`; update it — see Step 4):

```rust
#[test]
fn lowers_collections_and_index() {
    let hir = lower("func f(): #[int] = #[1, 2, 3]");
    assert_eq!(
        hir.funcs[0].body,
        HirExpr::Array(vec![HirExpr::Num(1.0), HirExpr::Num(2.0), HirExpr::Num(3.0)]),
    );

    let hir = lower(r#"func g(): int = #{ "a": 1 }["a"]"#);
    assert!(matches!(hir.funcs[0].body, HirExpr::MapGet { .. }), "map index → MapGet");

    let hir = lower("func h(): int = #[10, 20][1]");
    assert!(matches!(hir.funcs[0].body, HirExpr::Index { .. }), "list index → Index");
}
```

- [ ] **Step 3: Run; expect failure**

Run: `cargo +nightly test -p nymph-sema --test lower_hir lowers_collections_and_index`
Expected: FAIL to compile — `lower` helper signature, and the new HIR variants aren't produced.

- [ ] **Step 4: Thread the interner into lowering**

Change `lower_hir` to `pub fn lower_hir(module: &Module, checked: &Checked) -> HirModule`. Thread `&Checked` (or the `&Annotations` + `&Interner` it needs) down to `lower_expr` (store them in a small `Lowerer<'a>` struct, or pass references through — a struct is cleaner as the recursion deepens). Update `lower_hir`'s doc to note it now consumes types.

Add the lowering arms (in `lower_expr`):

```rust
ExprKind::Tuple(items) => HirExpr::Array(lower_items(items)),
ExprKind::List(items) => HirExpr::Array(lower_items(items)),
ExprKind::Map(entries) => HirExpr::MapLit(lower_map_entries(entries)),
ExprKind::IndexAccess { parent, index, .. } => {
    // Dispatch on the receiver's recorded type: Map → get, else subscript.
    let recv = lower_expr(parent);
    let index = lower_expr(index);
    let recv_is_map = self
        .annotations
        .get(parent.id)
        .is_some_and(|info| matches!(self.interner.kind(info.ty), TyKind::Map(..)));
    if recv_is_map {
        HirExpr::MapGet { recv: Box::new(recv), key: Box::new(index) }
    } else {
        HirExpr::Index { recv: Box::new(recv), index: Box::new(index) }
    }
}
```

`lower_items` maps each `ListItem::Expr(e)` to `lower_expr(e)` and `panic!`s on `ListItem::Spread` (2A limitation). `lower_map_entries` maps `MapEntry::Entry(k, v)` to `(lower_expr(k), lower_expr(v))` and `panic!`s on `MapEntry::Spread`. Verify `ListItem`/`MapEntry` shapes in `crates/nymph-ast/src/expr.rs`.

> The `lower_expr` free functions from Slice 1 become methods on the `Lowerer` (they need `self.annotations`/`self.interner`). Convert them; keep the logic identical.

- [ ] **Step 5: Update the test `lower` helper and the callers**

The `lower_hir.rs` test helper must build a `Checked` and pass it:

```rust
fn lower(src: &str) -> HirModule {
    let parsed = parse_module(src, "test");
    assert!(!parsed.diagnostics.iter().any(|d| d.is_error()), "parse failed");
    let checked = check_module(&parsed.tree);
    assert!(checked.diags.is_empty(), "check failed: {:?}", checked.diags);
    nymph_sema::lower_hir(&parsed.tree, &checked)
}
```

Update `nymph_codegen::compile` and `run_node.rs`'s `compile` helper to pass `&checked` to `lower_hir`.

- [ ] **Step 6: Run the lowering tests + build**

Run: `cargo +nightly test -p nymph-sema --test lower_hir && cargo +nightly build -p nymph-codegen`
Expected: PASS (codegen won't emit the new nodes yet — that's Task 4 — but it must compile; add temporary `unreachable!` arms for the new `HirExpr` variants in `emit.rs` if the match is non-exhaustive, to be filled in Task 4).

- [ ] **Step 7: Format, clippy, commit**

```bash
cargo +nightly fmt -p nymph-hir -p nymph-sema -p nymph-codegen
cargo +nightly clippy -p nymph-hir -p nymph-sema --all-targets
git add crates/nymph-hir crates/nymph-sema crates/nymph-codegen
git commit -m "feat(sema): typed lower_hir; lower tuple/list/map literals + index dispatch"
```

---

## Task 4: Emit collections + run under Node

**Files:**
- Modify: `crates/nymph-codegen/src/emit.rs`
- Test: `crates/nymph-codegen/tests/run_node.rs`

**Interfaces:**
- Consumes: `HirExpr::{Array, MapLit, Index, MapGet}`.

- [ ] **Step 1: Write the failing Node-execution tests**

Add to `crates/nymph-codegen/tests/run_node.rs`:

```rust
#[test]
fn runs_list_and_index() {
    let src = "func third(): int = #[10, 20, 30][2]";
    assert_eq!(run(src, "third()"), "30");
}

#[test]
fn runs_tuple_roundtrip() {
    let src = "func pair(): #(int, int) = #(1, 2)";
    // A tuple emits as a JS array.
    assert_eq!(run(src, "JSON.stringify(pair())"), "[1,2]");
}

#[test]
fn runs_map_get() {
    let src = r#"func lookup(): int = #{ "a": 5, "b": 6 }["b"]"#;
    assert_eq!(run(src, "lookup()"), "6");
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-codegen --test run_node runs_list_and_index`
Expected: FAIL — the new HIR variants hit the `unreachable!` from Task 3 Step 6.

- [ ] **Step 3: Implement the emitters**

In `crates/nymph-codegen/src/emit.rs`, `emit_expr`, replace the temporary `unreachable!` arms:

```rust
HirExpr::Array(items) => {
    let mut elems = self.ast.vec();
    for item in items {
        elems.push(ArrayExpressionElement::from(self.emit_expr(item)));
    }
    self.ast.expression_array(SPAN, elems, None)
}
HirExpr::MapLit(pairs) => {
    // new Map([[k, v], …])
    let mut entries = self.ast.vec();
    for (k, v) in pairs {
        let mut pair = self.ast.vec();
        pair.push(ArrayExpressionElement::from(self.emit_expr(k)));
        pair.push(ArrayExpressionElement::from(self.emit_expr(v)));
        let arr = self.ast.expression_array(SPAN, pair, None);
        entries.push(ArrayExpressionElement::from(arr));
    }
    let outer = self.ast.expression_array(SPAN, entries, None);
    // `new Map(<outer>)`
    let callee = self.ast.expression_identifier(SPAN, "Map");
    let mut args = self.ast.vec();
    args.push(Argument::from(outer));
    self.ast.expression_new(SPAN, callee, oxc::ast::NONE, args)
}
HirExpr::Index { recv, index } => {
    let object = self.emit_expr(recv);
    let property = self.emit_expr(index);
    Expression::ComputedMemberExpression(
        self.ast.alloc_computed_member_expression(SPAN, object, property, false),
    )
}
HirExpr::MapGet { recv, key } => {
    // recv.get(key)
    let object = self.emit_expr(recv);
    let member = Expression::StaticMemberExpression(self.ast.alloc_static_member_expression(
        SPAN,
        object,
        self.ast.identifier_name(SPAN, "get"),
        false,
    ));
    let mut args = self.ast.vec();
    args.push(Argument::from(self.emit_expr(key)));
    self.ast.expression_call(SPAN, member, oxc::ast::NONE, args, false)
}
```

> Verify against oxc 0.138: `expression_array`, `ArrayExpressionElement::from`, `expression_new`, `alloc_computed_member_expression`, `alloc_static_member_expression`, `identifier_name`. Model the member expressions on the reference emitter (`assign_target_computed_member` / `member` helpers, lines ~128 & ~161). Adjust any signature the compiler rejects.

- [ ] **Step 4: Run the execution tests**

Run: `cargo +nightly test -p nymph-codegen --test run_node`
Expected: PASS — `third()`→30, `JSON.stringify(pair())`→`[1,2]`, `lookup()`→6, plus all Slice 1 tests.

- [ ] **Step 5: Full workspace gate**

Run: `cargo +nightly test && cargo +nightly clippy --all-targets`
Expected: PASS/clean (ignore pre-existing warnings in the error-code crates you don't own).

- [ ] **Step 6: Format, commit**

```bash
cargo +nightly fmt -p nymph-codegen
git add crates/nymph-codegen
git commit -m "feat(codegen): emit tuples/lists (arrays), maps (Map), and index access; run under Node"
```

---

## Self-Review

**Spec coverage (against the design's "Slice 2 — Data types & the value ABI", collections subset):**
- Tuples → `HirExpr::Array` → JS array ✓
- Lists → `HirExpr::Array` → JS array ✓
- Maps → `HirExpr::MapLit` → `new Map([...])` ✓
- Index access → list/tuple `arr[i]` vs map `.get(k)`, dispatched in lowering via the interner ✓
- The interner-threading foundation (`Checked.interner`, typed `lower_hir`) that structs/enums/copy will reuse ✓

**Deferred to later Slice 2 parts (2B/2C), correctly:** structs (classes) + field access; enums + the Symbol tag ABI + the `equality.ts` update; defensive `Copy` for `mut` value-type bindings (needs a clear observable mutation path — resolve when planning 2C); spreads in collection literals; precise heterogeneous tuple-index typing; user `Index`/operator overloads on ADTs (Slice 4).

**Placeholder scan:** No "TBD"/"handle edge cases". The `record_resolution` `or_insert` wrinkle is called out with a concrete resolution (`and_modify` + `debug_assert`, no insert, since 2A never calls it). oxc 0.138 builder names are given at reference shapes with an explicit "verify by compiling" instruction (unavoidable — only the compiler pins 0.138 signatures). AST shape reads (`ListItem`/`MapEntry`, `IndexAccess`, `TyKind`) carry "verify against current code" notes because they are read, not guessed.

**Type consistency:** `Checked { diags, annotations, interner }`; `HirExpr::{Array(Vec<HirExpr>), MapLit(Vec<(HirExpr, HirExpr)>), Index { recv, index }, MapGet { recv, key }}`; `lower_hir(&Module, &Checked)`; `Annotations::record_resolution(NodeId, Resolution)` — names and signatures match across Tasks 1–4.

**Scope:** One coherent, Node-testable increment — the collection value forms plus the interner-threading foundation. Structs/enums/copy are separate follow-on plans, each independently testable.
