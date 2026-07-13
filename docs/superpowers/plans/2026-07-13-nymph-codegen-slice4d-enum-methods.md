# Slice 4D (Enum Methods — Prototype ABI) Implementation Plan

> **For agentic workers:** Executed by a single dynamic Workflow (investigate →
> implement TDD → review → adversarial refute → fix loop) **in the jj workspace
> at `/home/theonlytails/IdeaProjects/nymph_lang-ws-enums`** — all edits,
> cargo runs, and jj commands happen there. The controller commits and updates
> the ledger.

**Goal:** Methods on enums work end-to-end — inherent (`impl Color { func … }`),
interface impls (`impl Iface for Color { … }`, including materialized default
methods), and therefore operator dispatch on enum values. This closes the
4A-era deferral where enum impls type-check but lowering panics.

**Architecture:** A per-enum shared prototype object carries the methods;
variant values are created with `Object.create(<proto>)` so `c.m()` and `this`
work natively in JS with zero call-site rewriting. The tag ABI is unchanged
(`[TAG]` identity comparisons still work — prototypes don't affect own
properties). Enums WITHOUT methods keep today's exact emitted shape.

## Global Constraints

- Codegen stays type-free; deferred features panic loudly in lowering.
- Rust: `cargo +nightly`, hard tabs, 2-space width, clippy clean.
- Subagents do not commit; the controller commits with jj scoped commits.
- The tag-identity value ABI from Slice 2C must not change for existing tests.

## Current state (surveyed 2026-07-13 at c4106f12)

- Enums emit as an IIFE: `const E = (() => { const t0 = Symbol("E.V0"); …
  nullary variants are `{ [TAG]: t0 }` singletons, field variants are
  `(fields) => ({ [TAG]: t1, ...fields })` factories (emit.rs:349-476).
- The checker already type-checks enum impl bodies (members.rs; confirmed in
  Slice 4A — the 4A Critical was that LOWERING dropped them, now panics).
- lower_module collects impl methods into `methods_by_type` (by type name) and
  struct lowering consumes entries; leftovers hit the "non-struct types" assert
  (lower_hir.rs ~148) — enums land there today. Interface-default
  materialization (Slice 4C-b) rides the same collection (`push_impl_for_methods`
  / `push_unoverridden_defaults` + `assert_no_duplicate_methods`).
- Operators on ADTs record `UserImpl` via `dispatch_operator`; `is_adt` covers
  enum DefIds (verify) — so operator impls on enums should dispatch correctly
  once methods exist at runtime.

## Decisions

- **X1 (ABI):** enums WITH methods emit, inside the existing IIFE, a
  `const proto = { m1(…) { … }, … };` object holding all methods
  (`this`-based, same emit path as struct class methods if reusable), and
  every variant value is created with that prototype: nullary singletons via
  `Object.assign(Object.create(proto), { [TAG]: t_i })` (or equivalent), field
  factories via `(fields) => Object.assign(Object.create(proto), { [TAG]: t_i, ...fields })`.
  Enums without methods emit exactly today's shape (no proto, no
  Object.create) — zero churn for existing programs.
- **X2 (HIR/lowering):** `HirEnum` gains `methods: Vec<HirMethod>` (mirroring
  `HirClass.methods`). `lower_module` consumes `methods_by_type` entries for
  enums through the SAME path as structs: impl-provided methods in source
  order, then un-overridden interface defaults, then
  `assert_no_duplicate_methods`. The "non-struct types" assert remains for
  types that are neither struct nor enum (and for blanket/non-Reference
  targets, unchanged from 4C-b).
- **X3 (checker):** no checker changes expected — verify empirically that
  method calls, operator dispatch (`UserImpl`), and default-method resolutions
  on enum receivers already record correctly (they should; ADT machinery is
  type-constructor-agnostic). If something records wrongly, STOP and report
  rather than bolting on fixes.
- **X4 (`this` in enum methods):** `this` lowers to `HirExpr::This` (existing);
  in the prototype ABI `this` is the variant object, so `this.field` works on
  field variants and `this` itself can be matched (`match (this) { … }` — the
  match lowering is structural and tag-based; verify with a test).
- **X5 (out of scope, loud):** namespaced/static methods; `mut` methods;
  positional variant construction (existing panic); stdlib linkage; user
  `==`/`!=` dispatch (enum equality stays native tag/reference semantics).

## Tasks

### Task 1: HIR + lowering
Files: crates/nymph-hir/src/hir.rs (HirEnum.methods),
crates/nymph-sema/src/lower_hir.rs (enum consumption of methods_by_type,
struct-inner→enum-inner impl members if the AST allows them — verify what
enum-body member syntax exists), tests: crates/nymph-sema/tests/lower_hir.rs.
Cases: inherent method on enum lands in HirEnum.methods; ImplFor on enum with
default materialization; duplicate-name collision panics; struct behavior
unchanged.

### Task 2: emit
Files: crates/nymph-codegen/src/emit.rs, tests: the emit-level tests if any
exist for enums, else covered by Task 3.
Cases: method-less enum emits byte-identical shape to today (add a pin test if
cheap); enum with methods gets proto + Object.create in both variant shapes.

### Task 3: Node end-to-end
Files: crates/nymph-codegen/tests/run_node.rs (JS drivers: nullary variant
`E.V0`, field variant `E.V1({ n: 3 })` — verify the exact JS construction
shape used by existing enum tests).
Cases: inherent method on a nullary variant (match/if on `this` inside);
method reading `this.field` on a field variant; operator impl on an enum
(`a + b` → `.plus()`); interface default method on an enum; tag identity still
works (existing enum tests keep passing).

### Task 4 (controller): commit, ledger, record review outcome.
