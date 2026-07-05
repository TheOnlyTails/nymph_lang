# Nymph Codegen — Slice 0 (Foundation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lay the foundation the JS code generator needs: give AST expressions stable `NodeId` identity, create the `nymph-hir` crate and move the interned type model into it, and make the checker record its per-expression decisions into an annotation side-table returned to callers.

**Architecture:** This is Slice 0 of the codegen design (`docs/superpowers/specs/2026-07-05-nymph-codegen-design.md`). It produces no JavaScript yet — its deliverable is verified by existing sema/parser tests staying green plus new tests for node identity and annotation recording. It converts the `Expr` enum into a self-spanned `struct Expr { kind, span, id }` wrapper, relocates `Ty`/ids into `nymph-hir`, and adds an `Annotations` map keyed by `NodeId`.

**Tech Stack:** Rust (edition 2024, nightly toolchain), a cargo workspace under `crates/`. Existing crates: `nymph-ast`, `nymph-syntax`, `nymph-diagnostics`, `nymph-sema`. New crate this plan: `nymph-hir`. Dependencies already in the workspace: `ecow`, `ordered-float`, `strum`, `rustc-hash` (`FxHashMap`), `salsa`, `chumsky`.

## Global Constraints

- **Toolchain:** every cargo command MUST be prefixed `cargo +nightly` — the shell exports `RUSTUP_TOOLCHAIN=1.96.0` (stable), which overrides `rust-toolchain.toml`. Plain `cargo` fails with `#![feature] may not be used on the stable release channel`.
- **Edition:** 2024, inherited from `[workspace.package]`.
- **Every AST node type keeps its `#[derive(..., salsa::Update)]`** — the incremental DB stores whole trees; dropping `Update` breaks the driver layer later.
- **Formatting/lints:** finish each task with `cargo +nightly fmt` and `cargo +nightly clippy --all-targets` clean (the codebase is clippy-clean today; keep it that way).
- **No behavior change to type checking** in this slice: the set of diagnostics produced for any program must be identical before and after. Existing sema tests are the regression harness.
- **Interner note:** `Ty` is a `Copy` index into an `Interner`; handles are only valid relative to the interner that minted them. Moving the type does not change this invariant.

---

## File Structure

- `crates/nymph-ast/src/lib.rs` — add `NodeId` newtype.
- `crates/nymph-ast/src/expr.rs` — split `Expr` into `ExprKind` + self-spanned `struct Expr`; ripple `Spanned<Expr>`→`Expr` and `Box<Spanned<Expr>>`→`Box<Expr>` through the expr/statement/list/map/matcharm types.
- `crates/nymph-ast/src/decl.rs` — update the ~5 `Spanned<Expr>` body/value fields to `Expr`.
- `crates/nymph-ast/src/*` (Display impls) — match on `expr.kind`.
- `crates/nymph-syntax/src/parser/{mod,expr,decl,pattern}.rs` — thread an id counter, build `ExprKind` via a `mk_expr` helper, return `Expr` instead of `Spanned<Expr>`.
- `crates/nymph-hir/` — **new crate**: `Cargo.toml`, `src/lib.rs`, `src/ty/{mod,fold}.rs` (moved), `src/ids.rs` (moved).
- `crates/nymph-sema/src/{ty,ids}` — removed; replaced by re-exports from `nymph-hir` in `lib.rs`.
- `crates/nymph-sema/src/annotate.rs` — **new**: `NodeId`-keyed `Annotations`, `ExprInfo`, `Resolution`, `DispatchKind`, and the `Checked` result struct.
- `crates/nymph-sema/src/check.rs` — `check_module`/`check_program` return `Checked`; `Checker` grows an `annotations` field and a `record` helper.
- `crates/nymph-sema/src/infer_expr.rs` — call `record` at decision sites (this slice: literals + binary operators, enough to prove the mechanism; later slices extend coverage).
- `Cargo.toml` (workspace) — add `crates/nymph-hir` to `members` and to `[workspace.dependencies]`.

---

## Task 1: `NodeId` type and the `Expr`/`ExprKind` split (nymph-ast)

**Files:**
- Modify: `crates/nymph-ast/src/lib.rs` (add `NodeId`)
- Modify: `crates/nymph-ast/src/expr.rs` (split `Expr`, ripple wrapper types)
- Modify: `crates/nymph-ast/src/decl.rs` (`Spanned<Expr>` → `Expr` in body/value fields)

**Interfaces:**
- Produces:
  - `nymph_ast::NodeId(pub u32)` with `NodeId::DUMMY == NodeId(u32::MAX)`, derives `Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, salsa::Update`.
  - `nymph_ast::expr::Expr { pub kind: ExprKind, pub span: Span, pub id: NodeId }`.
  - `nymph_ast::expr::ExprKind` — every variant the old `Expr` enum had, with `Box<Spanned<Self>>` fields becoming `Box<Expr>` and `Spanned<Expr>` fields becoming `Expr`.
  - Convenience: `impl Expr { pub fn new(kind: ExprKind, span: Span, id: NodeId) -> Self }`.

- [ ] **Step 1: Add the `NodeId` newtype**

In `crates/nymph-ast/src/lib.rs`, after the `Span` definition, add:

```rust
/// Stable identity for an AST expression node, assigned once by the parser in
/// construction order. Distinct from [`Span`]: two nodes can share text but never
/// an id. Used to key semantic annotations (resolved types, operator impl
/// selections) that later passes read back.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, salsa::Update)]
pub struct NodeId(pub u32);

impl NodeId {
    /// A placeholder id for nodes built outside the parser (tests, synthetic
    /// nodes). Never assigned by the parser, so it never collides with a real id.
    pub const DUMMY: NodeId = NodeId(u32::MAX);
}
```

- [ ] **Step 2: Split `Expr` into `ExprKind` + `Expr` wrapper**

In `crates/nymph-ast/src/expr.rs`, rename the existing `pub enum Expr { ... }` to `pub enum ExprKind { ... }`. Inside it, change every `Box<Spanned<Self>>` to `Box<Expr>`, every `Spanned<Self>`/`Spanned<Expr>` to `Expr`. The recursive `Self` now refers to `ExprKind`, so spell the child type as `Expr` explicitly. For example the binary/call/if variants become:

```rust
Call {
    func: Box<Expr>,
    generics: Vec<Spanned<GenericArg>>,
    args: Vec<Spanned<CallArg>>,
},
BinaryOp {
    lhs: Box<Expr>,
    op: BinaryOperator,
    rhs: Box<Expr>,
},
If {
    condition: Box<Expr>,
    then: Box<Expr>,
    otherwise: Option<Box<Expr>>,
},
Match {
    value: Box<Expr>,
    arms: Vec<MatchArm>,
},
Block {
    body: Vec<Spanned<Statement>>,
    label: Option<Ident>,
},
```

(`Spanned<Pattern>`, `Spanned<Type>`, `Spanned<GenericArg>`, `Ident` fields stay as they are — only expression children change.) Keep `#[derive(Clone, Debug, PartialEq, salsa::Update)]` on `ExprKind`. Then add the wrapper directly above it:

```rust
/// A self-spanned expression node: its kind, the source span it covers, and a
/// stable [`NodeId`]. Expressions carry their own span (unlike other AST nodes,
/// which are wrapped in [`Spanned`]) so that identity, position, and shape travel
/// together — the shape the HIR and LSP both want.
#[derive(Clone, Debug, PartialEq, salsa::Update)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    pub id: NodeId,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span, id: NodeId) -> Self {
        Self { kind, span, id }
    }
}
```

Add `NodeId` to the `use crate::{...}` import at the top of `expr.rs`.

- [ ] **Step 3: Ripple the wrapper change through the sibling types in `expr.rs`**

In the same file, replace `Spanned<Expr>` with `Expr` in: `Statement::Expr`, `Statement::Let.value`, `StringPart::InterpolatedExpr`, `ListItem::Expr`, `ListItem::Spread`, `MapEntry::Entry` (both), `MapEntry::Spread`, `RangeKind` variants (`Box<Spanned<Expr>>` → `Box<Expr>`), `ClosureParam` has none, `CallArg.value`, `MatchArm.guard` (`Option<Spanned<Expr>>` → `Option<Expr>`) and `MatchArm.body`. Leave every `Spanned<Pattern>` and `Spanned<Type>` untouched.

- [ ] **Step 4: Ripple into `decl.rs`**

In `crates/nymph-ast/src/decl.rs`, change each `Spanned<Expr>` field to `Expr` (the function/let body and value fields, the struct/enum default and member body/value fields — the lines around 30, 38, 150, 175, 181, 204, 208). Add/adjust the `use crate::expr::Expr;` import if needed.

- [ ] **Step 5: Build nymph-ast; expect Display errors**

Run: `cargo +nightly build -p nymph-ast`
Expected: FAIL — compile errors only in the `Display`/formatting impls that match on the old `Expr` variants (they now need `&expr.kind`). Type definitions themselves compile.

- [ ] **Step 6: Fix the Display impls**

Wherever a `Display`/formatter matches `Expr` variants (search `crates/nymph-ast/src` for `Expr::` and `match ` over expressions), change the scrutinee to `&self.kind` (for `impl Display for Expr`) or `&expr.kind` (when formatting a child), and change child recursion from `spanned.0`/`spanned.value()` to the plain `Expr`. A binary-op arm, for instance, goes from matching `Expr::BinaryOp { lhs, op, rhs }` and printing `lhs.0` to matching `ExprKind::BinaryOp { lhs, op, rhs }` and printing `lhs` (which is now `&Expr`, itself `Display`). Any place that printed a `Spanned<Expr>` child now prints an `Expr` child directly.

- [ ] **Step 7: Build and format**

Run: `cargo +nightly build -p nymph-ast && cargo +nightly fmt && cargo +nightly clippy -p nymph-ast --all-targets`
Expected: PASS, clippy clean.

- [ ] **Step 8: Commit**

```bash
git add crates/nymph-ast
git commit -m "refactor(ast): self-spanned Expr with NodeId (ExprKind + wrapper)"
```

---

## Task 2: Parser assigns NodeIds and builds `ExprKind`

**Files:**
- Modify: `crates/nymph-syntax/src/parser/mod.rs` (id counter + `mk_expr` helper)
- Modify: `crates/nymph-syntax/src/parser/expr.rs` (build `ExprKind`, return `Expr`)
- Modify: `crates/nymph-syntax/src/parser/decl.rs`, `pattern.rs` (call sites that build/return expressions)
- Test: `crates/nymph-syntax/tests/parser.rs` (new node-id test)

**Interfaces:**
- Consumes: `nymph_ast::expr::{Expr, ExprKind}`, `nymph_ast::NodeId`.
- Produces: `Parser::mk_expr(&mut self, kind: ExprKind, span: Span) -> Expr` assigning a fresh, monotonically increasing `NodeId`; `parse_expr`/`parse_bp`/`parse_prefix` and friends now return `Expr` (not `Spanned<Expr>`).

- [ ] **Step 1: Write the failing test for node-id uniqueness**

Add to `crates/nymph-syntax/tests/parser.rs` (adjust the parse-entry helper name to match the file's existing helper — look for how other tests obtain a parsed module):

```rust
#[test]
fn expression_node_ids_are_unique_and_dense() {
    // A body with several nested expressions: binary ops, a call, a literal.
    let src = "func f() = 1 + g(2) * 3";
    let module = parse_module_ok(src); // existing test helper that returns a Module

    let mut ids = Vec::new();
    collect_expr_ids(&module, &mut ids); // helper defined in this test file

    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "node ids must be unique");
    assert_ne!(ids.len(), 0, "expected several expression nodes");
    // Dense from 0: the parser numbers in construction order starting at 0.
    assert_eq!(*sorted.first().unwrap(), 0);
    assert_eq!(*sorted.last().unwrap(), (ids.len() as u32) - 1);
}
```

Add a small recursive `collect_expr_ids(module, out)` walker in the test module that pushes `expr.id.0` for every `Expr` it reaches (walk function bodies; recurse through `ExprKind` children). Keep it minimal — it only needs to reach the nodes in the test source (binary ops, call, args, literals).

- [ ] **Step 2: Run the test; expect it not to compile / fail**

Run: `cargo +nightly test -p nymph-syntax expression_node_ids_are_unique_and_dense`
Expected: FAIL to compile — the parser still returns `Spanned<Expr>` and there is no `mk_expr` yet.

- [ ] **Step 3: Add the id counter and `mk_expr` to the parser**

In `crates/nymph-syntax/src/parser/mod.rs`, add a `next_id: u32` field to the `Parser` struct (initialise to `0` wherever the parser is constructed). Add:

```rust
impl Parser<'_> {
    /// Build a self-spanned expression, assigning the next fresh node id.
    pub(super) fn mk_expr(&mut self, kind: nymph_ast::expr::ExprKind, span: nymph_ast::Span) -> nymph_ast::expr::Expr {
        let id = nymph_ast::NodeId(self.next_id);
        self.next_id += 1;
        nymph_ast::expr::Expr { kind, span, id }
    }
}
```

- [ ] **Step 4: Convert the expression build sites**

In `crates/nymph-syntax/src/parser/expr.rs`, change every `Spanned(Expr::Variant { .. }, span)` (and `Expr::Variant(..).spanned(span)` if present) to `self.mk_expr(ExprKind::Variant { .. }, span)`. Change return types of `parse_expr`, `parse_bp`, `parse_prefix`, and any helper returning `Spanned<Expr>` to `Expr`. Where a child expression's span was read as `child.1`/`child.span()`, read `child.span` now; where its value was `child.0`, use the `Expr` directly (e.g. `Box::new(lhs)` where `lhs: Expr`). Update the `use` line to import `ExprKind` alongside `Expr`.

- [ ] **Step 5: Fix ripple in decl.rs / pattern.rs**

Build to find the remaining call sites: `cargo +nightly build -p nymph-syntax`. Anywhere a parsed expression was stored into an AST field (function bodies, `let` values, default values, `MatchArm { body, guard }`, call args, list/map items, interpolated string parts, range bounds) now receives a plain `Expr` instead of `Spanned<Expr>` — drop the `Spanned(..)` wrapping / `.0`/`.1` accesses accordingly. For `MatchArm.guard`, an `Option<Spanned<Expr>>` becomes `Option<Expr>`.

- [ ] **Step 6: Build the crate**

Run: `cargo +nightly build -p nymph-syntax`
Expected: PASS.

- [ ] **Step 7: Run the new test and the existing parser suite**

Run: `cargo +nightly test -p nymph-syntax`
Expected: PASS — the new `expression_node_ids_are_unique_and_dense` test passes and all pre-existing parser tests still pass (parsing shape is unchanged; only the wrapper type differs, so any test comparing `Expr` values needs its expected values rebuilt via `mk_expr`-style construction or by comparing `.kind`). If existing tests construct `Expr` literals for comparison, update them to build `Expr { kind: ExprKind::..., span, id: NodeId::DUMMY }` and compare on `.kind` (add a note in the test that ids are excluded from the comparison).

- [ ] **Step 8: Format, clippy, commit**

```bash
cargo +nightly fmt && cargo +nightly clippy -p nymph-syntax --all-targets
git add crates/nymph-syntax
git commit -m "feat(parser): assign unique NodeIds to expressions via mk_expr"
```

---

## Task 3: Adapt nymph-sema to the new `Expr` shape

**Files:**
- Modify: `crates/nymph-sema/src/infer_expr.rs`, `check.rs`, and any other module matching on `Expr` variants (`exhaustive.rs`, `members.rs`, `lower.rs` may touch expression children).
- Test: existing `crates/nymph-sema/tests/*.rs` are the regression harness.

**Interfaces:**
- Consumes: `nymph_ast::expr::{Expr, ExprKind}`.
- Produces: no public API change in this task — purely adapts internal matches from `Expr::X` to `ExprKind::X` and reads `expr.span`/`expr.kind` where it used `spanned.1`/`spanned.0`.

- [ ] **Step 1: Build nymph-sema to enumerate the breakage**

Run: `cargo +nightly build -p nymph-sema`
Expected: FAIL — every `match expr.value()`/`match &spanned.0` over `Expr` variants and every `Expr::Variant` pattern is now a type error (the scrutinee is `Expr`, patterns name `ExprKind`).

- [ ] **Step 2: Convert the match sites**

For each error: where the checker previously received `&Spanned<Expr>` and matched `&spanned.0` / `spanned.value()`, it now receives `&Expr`; match on `&expr.kind`. Replace `Expr::` with `ExprKind::` in patterns. Where it read the child span from `spanned.1`/`spanned.span()`, read `expr.span`. Where a function signature was `fn infer(&mut self, expr: &Spanned<Expr>, ...)`, change it to `fn infer(&mut self, expr: &Expr, ...)` and update callers (children are now `&Expr` directly, no `.as_ref()`/`.value()` needed). The 55 `Expr::` sites are mechanical; the compiler lists each one.

- [ ] **Step 3: Build until clean**

Run: `cargo +nightly build -p nymph-sema`
Expected: PASS.

- [ ] **Step 4: Run the full sema suite — the behavior gate**

Run: `cargo +nightly test -p nymph-sema`
Expected: PASS — all ~97 sema tests green, unchanged. This proves the refactor preserved checking behavior (Global Constraint: no diagnostic changes).

- [ ] **Step 5: Format, clippy, commit**

```bash
cargo +nightly fmt && cargo +nightly clippy -p nymph-sema --all-targets
git add crates/nymph-sema
git commit -m "refactor(sema): match on ExprKind for self-spanned Expr"
```

---

## Task 4: Create `nymph-hir` and move the type model into it

**Files:**
- Create: `crates/nymph-hir/Cargo.toml`, `crates/nymph-hir/src/lib.rs`
- Move: `crates/nymph-sema/src/ty/mod.rs` → `crates/nymph-hir/src/ty/mod.rs`; `crates/nymph-sema/src/ty/fold.rs` → `crates/nymph-hir/src/ty/fold.rs`; `crates/nymph-sema/src/ids.rs` → `crates/nymph-hir/src/ids.rs`
- Modify: `Cargo.toml` (workspace members + deps), `crates/nymph-sema/Cargo.toml` (depend on `nymph-hir`), `crates/nymph-sema/src/lib.rs` (re-export)

**Interfaces:**
- Produces: `nymph_hir::ty::{Ty, TyKind, GenericArgs, Interner}` and `nymph_hir::ids::{DefId, ParamIdx, InferVar}`, with the exact same definitions they have today. `nymph-sema` re-exports them so existing `crate::ty::*` / `crate::ids::*` paths inside sema keep resolving.

- [ ] **Step 1: Create the crate manifest**

Create `crates/nymph-hir/Cargo.toml` (mirror `crates/nymph-sema/Cargo.toml`'s header style):

```toml
[package]
name = "nymph-hir"
version.workspace = true
edition.workspace = true
license.workspace = true
homepage.workspace = true
repository.workspace = true
authors.workspace = true

[dependencies]
ecow.workspace = true
rustc-hash.workspace = true
ordered-float.workspace = true
```

(Check the exact dependency keys used by `nymph-sema`'s manifest and match them; `ty/mod.rs` uses `ecow::EcoString` and `rustc_hash::FxHashMap`. Add `ordered-float` only if `ty` or `fold` reference it — otherwise omit.)

- [ ] **Step 2: Register the crate in the workspace**

In the root `Cargo.toml`, add `"crates/nymph-hir",` to `members` (uncomment/insert it before `nymph-codegen`), and confirm `[workspace.dependencies]` already has the `nymph-hir = { path = "crates/nymph-hir" }` line (it does per the current file) — keep it.

- [ ] **Step 3: Move the files**

```bash
mkdir -p crates/nymph-hir/src/ty
git mv crates/nymph-sema/src/ty/mod.rs crates/nymph-hir/src/ty/mod.rs
git mv crates/nymph-sema/src/ty/fold.rs crates/nymph-hir/src/ty/fold.rs
git mv crates/nymph-sema/src/ids.rs crates/nymph-hir/src/ids.rs
```

- [ ] **Step 4: Write `nymph-hir/src/lib.rs`**

```rust
//! The interned semantic type model and stable identity handles shared between the
//! type checker (`nymph-sema`, which produces types) and later passes (lowering and
//! code generation, which consume them). Kept in its own crate so neither side
//! depends on the other's logic.

pub mod ids;
pub mod ty;

pub use ids::{DefId, InferVar, ParamIdx};
pub use ty::{GenericArgs, Interner, Ty, TyKind};
```

In the moved `ty/mod.rs`, change `use crate::ids::{DefId, InferVar, ParamIdx};` — it already says `crate::ids`, which now resolves within `nymph-hir`, so it is correct as-is. Verify `ty/fold.rs`'s imports similarly reference `crate::ids` / `crate::ty` and leave them.

- [ ] **Step 5: Build the new crate in isolation**

Run: `cargo +nightly build -p nymph-hir`
Expected: PASS.

- [ ] **Step 6: Point nymph-sema at nymph-hir**

In `crates/nymph-sema/Cargo.toml`, add `nymph-hir.workspace = true` under `[dependencies]`. In `crates/nymph-sema/src/lib.rs`, delete the `pub mod ids;` and `pub mod ty;` module declarations and replace the `pub use ids::{...}` / `pub use ty::{...}` lines with re-exports from the new crate:

```rust
pub use nymph_hir::ids::{self, DefId, InferVar, ParamIdx};
pub use nymph_hir::ty::{self, GenericArgs, Interner, Ty, TyKind};
```

This keeps every intra-crate `crate::ty::…` and `crate::ids::…` path working (they resolve through the re-exported modules).

- [ ] **Step 7: Build the workspace and run all tests**

Run: `cargo +nightly build && cargo +nightly test`
Expected: PASS — nothing observable changed; the type model just lives elsewhere. Fix any now-ambiguous imports (e.g. a module importing both `crate::ty` and `nymph_hir::ty` — pick one).

- [ ] **Step 8: Format, clippy, commit**

```bash
cargo +nightly fmt && cargo +nightly clippy --all-targets
git add -A
git commit -m "refactor: extract nymph-hir crate with the Ty model and id handles"
```

---

## Task 5: Annotation types and the `Checked` result

**Files:**
- Create: `crates/nymph-sema/src/annotate.rs`
- Modify: `crates/nymph-sema/src/lib.rs` (declare + export), `crates/nymph-sema/src/check.rs` (return `Checked`, add `Checker.annotations`)
- Test: `crates/nymph-sema/tests/annotate.rs` (new)

**Interfaces:**
- Produces:
  - `nymph_sema::annotate::DispatchKind { BuiltinEager, BuiltinShortCircuit, UserImpl }` (derives `Clone, Copy, PartialEq, Eq, Debug`).
  - `nymph_sema::annotate::Resolution { pub method: DefId, pub dispatch: DispatchKind }` (`Clone, Copy, PartialEq, Debug`).
  - `nymph_sema::annotate::ExprInfo { pub ty: Ty, pub resolution: Option<Resolution> }` (`Clone, Copy, Debug`).
  - `nymph_sema::annotate::Annotations` — a newtype over `FxHashMap<NodeId, ExprInfo>` with `get(NodeId) -> Option<ExprInfo>` and `pub(crate) fn record(&mut self, id: NodeId, info: ExprInfo)`.
  - `nymph_sema::Checked { pub diags: Vec<Diagnostic>, pub annotations: Annotations }`.
  - `check_module(&Module) -> Checked` and `check_program(&[Module]) -> Checked` (return type changed from `Vec<Diagnostic>`).

- [ ] **Step 1: Write the failing test**

Create `crates/nymph-sema/tests/annotate.rs`:

```rust
use nymph_sema::check_module;
// Use whatever helper the other sema integration tests use to parse a source
// string into a `Module` (see tests/check.rs for the shared parse helper; if it is
// a local fn, replicate the few lines that lex+parse here).

#[test]
fn checked_result_exposes_diags_and_annotations() {
    let module = parse("func f(): int = 1 + 2");
    let checked = check_module(&module);
    assert!(checked.diags.is_empty(), "well-typed program has no diagnostics");
    // At least one expression was annotated with a type (the mechanism is wired).
    assert!(
        checked.annotations.len() > 0,
        "checker should record at least one expression annotation"
    );
}
```

Add `pub fn len(&self) -> usize` to `Annotations` for the assertion.

- [ ] **Step 2: Run it; expect a compile failure**

Run: `cargo +nightly test -p nymph-sema --test annotate`
Expected: FAIL to compile — `Checked`, `.diags`, `.annotations` do not exist; `check_module` still returns `Vec<Diagnostic>`.

- [ ] **Step 3: Write `annotate.rs`**

```rust
//! The side-table of per-expression decisions the checker records for the lowering
//! pass. Keyed by [`NodeId`] so the lowering can look up, for each AST expression,
//! its resolved type and (for desugared operators/casts/calls) which impl was
//! selected and how it must be dispatched in codegen.

use nymph_ast::NodeId;
use nymph_diagnostics::Diagnostic;
use rustc_hash::FxHashMap;

use crate::{DefId, Ty};

/// How a resolved operator/method call must be emitted by codegen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DispatchKind {
    /// A built-in primitive operation emitted as a native JS operator.
    BuiltinEager,
    /// A built-in default whose semantics short-circuit (`&&`, `||`, `??`),
    /// lowered to lazy control flow rather than an eager call.
    BuiltinShortCircuit,
    /// A user-provided interface impl: an ordinary eager method/function call.
    UserImpl,
}

/// The resolved callee behind a desugared operator, cast, index, or method call.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Resolution {
    pub method: DefId,
    pub dispatch: DispatchKind,
}

/// What the checker learned about one expression node.
#[derive(Clone, Copy, Debug)]
pub struct ExprInfo {
    pub ty: Ty,
    pub resolution: Option<Resolution>,
}

/// A [`NodeId`]-keyed map of [`ExprInfo`], produced by checking and consumed by
/// lowering.
#[derive(Clone, Debug, Default)]
pub struct Annotations(FxHashMap<NodeId, ExprInfo>);

impl Annotations {
    pub fn get(&self, id: NodeId) -> Option<ExprInfo> {
        self.0.get(&id).copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn record(&mut self, id: NodeId, info: ExprInfo) {
        // DUMMY-id nodes (built outside the parser) are never annotated.
        if id != NodeId::DUMMY {
            self.0.insert(id, info);
        }
    }
}

/// The full result of checking: diagnostics plus the annotation side-table. When
/// `diags` contains errors, `annotations` may be incomplete and lowering is skipped.
#[derive(Clone, Debug)]
pub struct Checked {
    pub diags: Vec<Diagnostic>,
    pub annotations: Annotations,
}
```

- [ ] **Step 4: Declare and export the module**

In `crates/nymph-sema/src/lib.rs`, add `mod annotate;` and extend the public exports:

```rust
pub use annotate::{Annotations, Checked, DispatchKind, ExprInfo, Resolution};
pub use check::{check_module, check_program};
```

- [ ] **Step 5: Give the `Checker` an annotations field and return `Checked`**

In `crates/nymph-sema/src/check.rs`, add `pub(crate) annotations: crate::annotate::Annotations,` to the `Checker` struct and initialise it to `Annotations::default()` wherever the `Checker` is constructed. Change `check_module` and `check_program` to build the `Checker`, run the existing pipeline, and return:

```rust
Checked { diags: checker.diags, annotations: checker.annotations }
```

instead of `checker.diags`. (Follow the existing construction/return flow in those two functions; only the final value and return type change.)

- [ ] **Step 6: Update existing callers of `check_module`/`check_program`**

Existing sema tests call `check_module(&m)` expecting `Vec<Diagnostic>`. Update them to `check_module(&m).diags`. Find them: `grep -rn "check_module\|check_program" crates/nymph-sema/tests`. This is a mechanical `.diags` suffix at each call site.

- [ ] **Step 7: Run the new test and the whole suite**

Run: `cargo +nightly test -p nymph-sema`
Expected: the new `annotate` test FAILS on the `annotations.len() > 0` assertion (nothing records yet) but COMPILES; all other tests pass with the `.diags` suffix. This confirms the plumbing is correct and isolates the remaining work to Task 6.

- [ ] **Step 8: Commit the plumbing (test left red intentionally)**

```bash
cargo +nightly fmt && cargo +nightly clippy -p nymph-sema --all-targets
git add crates/nymph-sema
git commit -m "feat(sema): Annotations side-table and Checked result (plumbing)"
```

---

## Task 6: Record annotations at checker decision sites

**Files:**
- Modify: `crates/nymph-sema/src/check.rs` (a `record` helper on `Checker`)
- Modify: `crates/nymph-sema/src/infer_expr.rs` (record at literal + binary-operator sites)
- Test: `crates/nymph-sema/tests/annotate.rs` (extend)

**Interfaces:**
- Consumes: `Checker.annotations`, `ExprInfo`, `Resolution`, `DispatchKind`, `Annotations::record`.
- Produces: `Checker::record(&mut self, id: NodeId, ty: Ty, resolution: Option<Resolution>)` — the single sema-internal entry point for annotating; later slices call it from every expression-kind site.

- [ ] **Step 1: Extend the test to assert on a specific node's recorded type**

Append to `crates/nymph-sema/tests/annotate.rs`:

```rust
#[test]
fn literal_and_operator_nodes_are_annotated() {
    let module = parse("func f(): int = 1 + 2");
    let checked = check_module(&module);
    assert!(checked.diags.is_empty());

    // Collect every expression id + its recorded type by walking the module and
    // looking each id up in the annotations. (Reuse a small walker like the one in
    // the parser test; here assert that the `+` expression and both int literals
    // resolved to the `int` type.) At minimum: the number of annotated nodes equals
    // the number of expression nodes reachable in the body.
    let ids = collect_expr_ids(&module);
    let annotated = ids.iter().filter(|id| checked.annotations.get(**id).is_some()).count();
    assert_eq!(annotated, ids.len(), "every expression node should be annotated");
}
```

(Provide `collect_expr_ids(&Module) -> Vec<NodeId>` in the test file, mirroring Task 2's walker but returning `NodeId`s. For this slice the body only contains literals and a binary op, so "every node" is those three.)

- [ ] **Step 2: Run it; expect failure**

Run: `cargo +nightly test -p nymph-sema --test annotate`
Expected: FAIL — `annotated` is 0, no sites record yet.

- [ ] **Step 3: Add the `record` helper to `Checker`**

In `crates/nymph-sema/src/check.rs`:

```rust
impl Checker<'_> {
    /// Record the checker's decision about an expression node so the lowering pass
    /// can read it back. `ty` should be the node's resolved type; `resolution` is
    /// set only for desugared operator/cast/method nodes.
    pub(crate) fn record(
        &mut self,
        id: nymph_ast::NodeId,
        ty: crate::Ty,
        resolution: Option<crate::annotate::Resolution>,
    ) {
        self.annotations
            .record(id, crate::annotate::ExprInfo { ty, resolution });
    }
}
```

- [ ] **Step 4: Record at the literal and binary-operator sites in `infer_expr.rs`**

Find where inference computes the type of an integer literal (`ExprKind::Int`) and of a binary operator (`ExprKind::BinaryOp`). At the point each returns its resolved `Ty`, call `self.record(expr.id, ty, resolution)`:
- Integer/other literals: `self.record(expr.id, ty, None);`
- Binary operator: after the operator method resolves, build a `Resolution { method, dispatch }` — use `DispatchKind::BuiltinEager` for the primitive fast-path, `DispatchKind::UserImpl` when it dispatched through `resolve_method`, and `DispatchKind::BuiltinShortCircuit` for the built-in `&&`/`||`/`??` default paths — then `self.record(expr.id, ty, Some(resolution))`. Where a primitive op has no `DefId` (pure built-in arithmetic), record `None` for the resolution but still record the type.

Keep the change surgical: this slice only needs literals and binary ops recorded to prove the mechanism. Recording the remaining expression kinds happens in slices 1–4 as their lowering is built, each calling the same `self.record(expr.id, ...)`.

- [ ] **Step 5: Run the annotate test**

Run: `cargo +nightly test -p nymph-sema --test annotate`
Expected: PASS — both annotate tests green (`func f(): int = 1 + 2` has three expression nodes, all annotated).

- [ ] **Step 6: Run the whole suite (no regressions)**

Run: `cargo +nightly test`
Expected: PASS — all crates, all tests.

- [ ] **Step 7: Format, clippy, commit**

```bash
cargo +nightly fmt && cargo +nightly clippy --all-targets
git add crates/nymph-sema
git commit -m "feat(sema): record type/dispatch annotations for literals and binary ops"
```

---

## Task 7: Update the memory note and mark Slice 0 done

**Files:**
- Modify: `~/.claude/projects/-home-theonlytails-IdeaProjects/memory/nymph-rewrite.md` and `MEMORY.md` (status update — done by the assistant, not a code change)

- [ ] **Step 1: Confirm the slice-0 deliverable end to end**

Run: `cargo +nightly build && cargo +nightly test && cargo +nightly clippy --all-targets && cargo +nightly fmt --check`
Expected: all PASS/clean. This is the slice acceptance: AST expressions carry unique `NodeId`s, `nymph-hir` owns the `Ty` model, and `check_*` returns `Checked { diags, annotations }` with literals and binary ops annotated.

- [ ] **Step 2: Record progress in memory**

Update `nymph-rewrite.md` with a short "Slice 0 (codegen foundation) DONE" note: self-spanned `Expr`/`ExprKind` + `NodeId`, `nymph-hir` crate holding `Ty`+ids, `Checked`/`Annotations` returned by the checker, annotation recording wired for literals+binary ops (other expr kinds recorded as later slices need them). Reference the spec and this plan by path.

---

## Self-Review

**Spec coverage (against `2026-07-05-nymph-codegen-design.md` "Sema changes" + "Slice 0"):**
- NodeIds on AST expression nodes, assigned by the parser → Tasks 1–2. ✓
- `nymph-hir` crate with the `Ty` model moved in, `nymph-sema` re-exports → Task 4. ✓
- `Annotations` (`NodeId → ExprInfo` with `Resolution`/`DispatchKind`), `check_*` returns diagnostics + annotations → Tasks 5–6. ✓
- Recording at existing decision sites (not new analysis); full expr-kind coverage deferred to later slices → Task 6 scopes to literals+binary ops with the shared `record` entry point. ✓
- Pattern `NodeId`s: the spec mentions patterns too, but lowering only needs them in Slice 3 (pattern matching); deferred there to keep this slice focused (YAGNI). Noted here so it is not lost.

**Placeholder scan:** No "TBD"/"handle edge cases"/"similar to Task N". Mechanical sweeps (Tasks 1/2/3) intentionally use "the compiler lists each site" + a `cargo build` gate rather than transcribing 135/55 identical edits — the concrete type definitions and representative conversions are given in full.

**Type consistency:** `NodeId(pub u32)`, `Expr { kind, span, id }`, `ExprKind`, `mk_expr(kind, span) -> Expr`, `Annotations::{get, record, len, is_empty}`, `ExprInfo { ty, resolution }`, `Resolution { method, dispatch }`, `DispatchKind`, `Checked { diags, annotations }`, `Checker::record(id, ty, resolution)`, `check_module/check_program -> Checked` — names and signatures match across Tasks 1–6.

**Scope:** One coherent slice (foundation). Produces no JS; the deliverable is the identity + type-model + annotation infrastructure, gated by the existing test suite plus new node-id and annotation tests. Slices 1–5 (HIR node types, lowering pass, and the oxc emitter that actually generate JavaScript) are separate plans, written after this lands.
