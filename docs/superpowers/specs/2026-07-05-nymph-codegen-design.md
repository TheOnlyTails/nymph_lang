# Nymph Codegen Layer — Design

**Date:** 2026-07-05
**Status:** Approved
**Layer:** 6 (JS codegen) of the Nymph rewrite. Follows the completed semantic
analysis layer (`nymph-sema`, Milestone B complete — the stdlib typechecks with
zero diagnostics).

## Goal

Turn a type-checked Nymph program into runnable ES module JavaScript, faithfully
honoring the language's deliberate semantic decisions (operator overloading with
short-circuit-vs-eager dispatch, value-vs-reference semantics with defensive
copies, literal widening). The acceptance test for the layer is that
`stdlib/src/*.nym` compiles end-to-end and runs under Node with zero runtime
errors.

## Background & constraints

The semantic analyzer (`nymph-sema`) currently exposes only
`check_module`/`check_program -> Vec<Diagnostic>`. It computes per-expression
types and per-operator impl selections *transiently* during inference and
discards them. Three locked design decisions are codegen concerns that each
need a semantic fact the AST alone does not carry:

- **Operator short-circuit vs eager** depends on *which impl was selected* (a
  built-in default short-circuits; a user overload is an eager method call).
- **Defensive copies** depend on *value-vs-reference type* of a binding.
- **`1f`/`1u` literal suffixes** depend on *inferred widening*.

There is already a JS **representation ABI** baked into the hand-written stdlib
`.ts` companions (e.g. `stdlib/src/ops/equality.ts`): enums as tagged objects,
structs as classes, lists/tuples as arrays, maps as `Map`. New codegen must emit
values in a compatible shape, updating those companions only where this design
deliberately changes the tag scheme (see ABI below).

## Architecture decision: typed IR (HIR)

Codegen consumes a **mid-level typed IR (HIR)**, produced by a lowering pass in
`nymph-sema`, not the raw AST. Rationale:

- Long-term, the IR is the stable contract that codegen — and any future second
  backend or optimization pass — speaks to.
- The IR erases the parts of the language that are genuinely redundant (every
  operator/cast/range/closure-shorthand has multiple surface forms for one
  meaning), which is where duplicated codegen logic and bugs would otherwise
  live.
- It stops at a *mid* level: it keeps the structured control flow (`if`/`while`/
  `match`/blocks, patterns) that JS can already express, because JS is a
  high-level target, not assembly. Lowering all the way to ANF would discard
  structure that maps 1:1 to JS and produce worse output.

## Crate layout & pipeline

```
nymph-ast ──► nymph-syntax ──► nymph-sema ──► nymph-codegen
    │                              │  ▲            │
    └──────────► nymph-hir ◄───────┘  │            │
                     ▲                │ (lowering   │ (consumes
                     └────────────────┘  produces    HIR only)
                                         HIR)
```

- **`nymph-hir`** (new crate): IR type definitions only, no logic —
  `HirModule`, `HirDecl`, `HirExpr`, `HirStmt`, `HirPat`, `HirArm`, etc. The
  interned type model (`Ty`, `TyKind`, `GenericArgs`, `Interner`) **moves here**
  from `nymph-sema` so both the producer and consumer can read types without
  depending on each other. `nymph-sema` re-exports it for continuity. (We use
  `nymph-hir` rather than a separate `nymph-types` crate to avoid crate sprawl.)
- **`nymph-sema`** gains a `lower_hir` module: `lower_hir(&Module, &Annotations)
  -> HirModule`, run after `check_*` succeeds.
- **`nymph-codegen`** (new crate): `emit(&HirModule) -> String`, pure HIR→JS via
  oxc. Never sees the AST or the checker.

## Sema changes: NodeIds & annotation recording

For lowering to read back the checker's decisions, AST nodes need stable
identity.

- Add a `NodeId` (`u32` newtype) to AST `Expr` and `Pattern` nodes (and binding
  sites that may need a `Copy`). Assigned by the **parser** via a monotonic
  counter on the cursor — additive to the AST structs.
- The `Checker` produces an `Annotations` output:
  `FxHashMap<NodeId, ExprInfo>` where `ExprInfo { ty: Ty, resolution:
  Option<Resolution> }`. `Resolution` records the resolved callee for desugared
  calls (`{ method: DefId, dispatch: DispatchKind }`) and constructor/variant
  resolutions. This is *recording at the existing decision sites* in
  `infer_expr`/`solve`, not new analysis.
- `check_module`/`check_program` return a struct carrying both diagnostics and
  annotations. Lowering runs only when diagnostics contain no errors.

## HIR shape

The guiding rule: **the IR removes things that have more than one surface form
for one meaning; it preserves things JS can already express.**

**Desugared away (gone in HIR):**

- Every operator → a resolved call node carrying the selected impl:
  `HirExpr::Call { callee, dispatch, args }` where `dispatch` is
  `BuiltinShortCircuit | BuiltinEager | UserImpl`. The dispatch tag comes
  straight from the recorded resolution; lowering never re-runs the solver.
- `as` / `??` / `in` / index → the same resolved-call node.
- Ranges (`a..b`, `a..=b`, `a..`, `..b`, `..=b`) → the matching stdlib struct
  constructor (`Range`, `RangeInclusive`, `RangeFrom`, `RangeTo`,
  `RangeToInclusive`), resolved to its `DefId` once and cached.
- Closure shorthands (`$0`/`$`/`$1`) → ordinary closures with synthesized
  params.
- Literal widening baked in: an `int` literal used as `float`/`uint` becomes
  `HirExpr::Lit(Float/Uint)` directly, so codegen needs no inference lookup.
- `this`/`self` made explicit as a distinct node.

**Kept structured (first-class HIR nodes):**

- `if`/`while`/`match`/block **as expressions**, each retaining a `Ty`. Codegen
  handles the JS statement-vs-expression lowering; the IR does not pre-flatten.
- `match`: `HirExpr::Match { scrutinee, arms: Vec<HirArm> }`, `HirArm { pat:
  HirPat, guard: Option<HirExpr>, body: HirExpr }`. `HirPat` is a typed tree —
  `Wildcard`, `Binding { name, sub: Option<Box<HirPat>> }`, `Variant { tag,
  fields }`, `Struct { fields }`, `Tuple`, `Lit`, `Range` — each carrying its
  `Ty`. Bindings can nest arbitrarily, so codegen compiles each arm from the
  *same* `HirPat` into two synchronized things: a **test** expression and a
  **binding sequence** (sub-value → name, by path). Exhaustiveness already ran
  in sema, so codegen may assume totality.
- Labels and `break value` preserved on loop/block nodes.

**Explicit nodes inserted by lowering:**

- `HirExpr::Copy(inner)` — a defensive copy. Needed only where JS's own value
  semantics don't already provide one: primitives and strings are already
  immutable-by-value in JS, so **only tuples** (arrays, which are mutable
  references) need copying on a `mut` value-type binding. Inserted at `let`/`mut`
  binding sites and assignments when the binding is `mut` and its `Ty` is
  (transitively) a tuple. Making it an explicit node keeps it assertable in
  lowering tests and keeps codegen a dumb printer.

**Carried as annotations on nodes:** resolved `Ty` everywhere; selected-impl
`DefId` + `DispatchKind` on every desugared call.

## Lowering pass (`nymph-sema::lower_hir`)

A recursive walk over the AST that, at each node, looks up its `NodeId` in
`Annotations` and produces the corresponding HIR node with its `Ty` attached.

- Entry: `lower_hir(&Module, &Annotations) -> HirModule` plus a `_program`
  variant flattening modules (mirroring `check_program`). Runs only on
  error-free programs.
- Operator/cast/index/`??`/`in` → read `Resolution`, emit `Call` with the
  recorded `DispatchKind`. Never re-runs the solver.
- Ranges → stdlib struct constructor `DefId` (cached), emit constructor call.
- `Copy` insertion per the rule above.
- Closure-shorthand normalization: synthesize params, rewrite references.
- Match: lower scrutinee + arms, each `HirPat` typed; leave test/binding
  compilation to codegen.

Independently testable: feed AST + annotations, assert on the HIR tree (e.g.
"`1 + 2.0` lowers to a `Call` to `Plus::plus` with `dispatch: BuiltinEager` and a
`Lit(Float 1.0)` arg").

## Codegen (`nymph-codegen`)

Pure `HirModule → String`, built on oxc's `AstBuilder` + `Codegen` (the proven
approach from the reference emitter, consuming HIR instead of AST+Context). An
`Emitter` builds an oxc `Program` and prints it — no string concatenation, so
output is always valid JS.

### Value ABI (the `.ts` contract, with Symbol tags)

The old `{ "~tag": "Included" }` string scheme has two collision surfaces: the
key `"~tag"` collides with a user field named `~tag`, and the value `"Included"`
collides across enums (so structural `equals` or an `is Included` test can match
the wrong type). JS `Symbol` closes both.

**Discriminant key** — one shared well-known symbol, the same everywhere so a
value built in module A is readable in module B:
`const TAG = Symbol.for("nymph.tag")`. The global registry is the correct tool
here precisely because the key must be *identical* across all modules/realms;
there is no collision concern for a symbol in our own reserved namespace, and as
a symbol key it is invisible to `Object.keys`/spread/JSON, so it never clashes
with user string fields.

**Variant tag values** — each variant gets a **unique** `Symbol(...)` (an
*unregistered* symbol, so identity is unique regardless of its label; the string
is only a debug label). Two enums with a same-named variant therefore get
distinct symbols even if their labels coincide — uniqueness does not depend on
qualifying the string. The symbol is stored canonically on the enum's variant
binding so any matcher can reach it:

```js
const TAG = Symbol.for("nymph.tag");
const Bound = {};
{
  const tIncluded = Symbol("Bound.Included");           // unique identity
  Bound.Included = Object.assign(
    (value) => ({ [TAG]: tIncluded, value }),           // factory
    { [TAG]: tIncluded },                               // ...also carries its tag
  );
  const tUnbounded = Symbol("Bound.Unbounded");
  Bound.Unbounded = Object.freeze({ [TAG]: tUnbounded }); // nullary singleton
}
```

- A field variant value: `{ [TAG]: tIncluded, value }`; the factory `Bound.Included`
  itself also carries `[TAG]: tIncluded`, so the symbol is reachable uniformly.
- A nullary variant is a frozen singleton whose `[TAG]` is its symbol.
- `x is Bound.Included` lowers to a pure **identity** test:
  `x?.[TAG] === Bound.Included[TAG]` — no string reconstruction. Because naming
  `Bound.Included` already requires the enum in scope, the matcher references the
  canonical symbol through the imported binding; cross-module matching needs no
  registry.
- Structs → JS classes; lists/tuples → arrays; maps → `Map` (unchanged).
- **Consequence:** the `.ts` companions that test the string `"~tag"` (chiefly
  `stdlib/src/ops/equality.ts`) are updated to read `[TAG]` and compare variant
  identity via the shared key. This is part of the codegen work.

### Emission behavior

- **Operator dispatch** reads `DispatchKind`: `BuiltinShortCircuit` → lazy JS
  (`a ? b : false`; `??` → match-style unwrap); `BuiltinEager` → native JS
  operator on primitives; `UserImpl` → an eager method/function call to the
  selected impl.
- **Control-flow-as-value:** `if`/`match`/`while`/block in expression position
  lower via result-temporary hoisting (`let _t; { ... _t = ...; }`); in
  statement position they stay statements. Labels and `break value` map to
  labeled statements assigning the temp.
- **Pattern compilation:** walk each `HirPat` once → `(test, bindings)`; `match`
  becomes an `if/else if` chain (totality guaranteed, so the last arm needs no
  test).
- **`Copy`** → `structuredClone(x)` (nested tuples) or a spread (flat).
- **External impls:** members whose body is `external` emit a call into the
  imported companion module (the `find_external_module` / `.external.ts`
  mechanism carried over from the reference).

`emit.rs` was ~3000 lines in the old tree; consuming pre-desugared HIR should
make the new emitter substantially smaller. If it still grows large, split by
concern (`emit/{decl,expr,pattern,value}.rs`) rather than one file.

## Milestones (vertical slices)

Each slice is independently testable and produces runnable JS, so we validate
against Node at every step. TDD throughout (test → red → implement → green).

- **Slice 0 — Prerequisite.** `NodeId`s in the AST + parser; `nymph-hir` crate
  with the `Ty` model moved in; `check_*` returns annotations. No JS yet;
  verified by existing sema tests still passing plus annotation-recording tests.
- **Slice 1 — Core expressions & functions.** Literals (with widening),
  identifiers, `let`/`mut`, calls, blocks, `if`/`while` (statement + value
  position), free functions. Milestone: arithmetic/control-flow programs run and
  print correct results under Node.
- **Slice 2 — Data types & the value ABI.** Structs, enums (Symbol tags),
  tuples, lists, maps, field/index access, defensive `Copy`. Milestone:
  round-trip every value form; update `equality.ts` to `[TAG]`; structural
  equality works.
- **Slice 3 — Pattern matching.** `HirPat` compilation: variants, structs,
  tuples, literals, ranges, nested bindings, guards, `is`/`!is`. Milestone:
  match-heavy option/result-style programs run correctly.
- **Slice 4 — Operators, methods, interfaces, ranges.** Dispatch kinds, method
  calls, impl/interface members as class methods, external companions, range
  constructors. Milestone: operator overloading and `.external.ts` wiring
  execute.
- **Slice 5 — Acceptance.** Compile `stdlib/src/*.nym` end-to-end and run a
  driver program exercising it under Node with zero runtime errors.

## Testing strategy

Each slice gets:

1. **Lowering unit tests** asserting on the HIR tree.
2. **Emit snapshot tests** asserting on JS text.
3. **Execution tests** that actually run the emitted JS under Node and assert on
   output — the one that catches real bugs.

## Non-goals (this layer)

- The salsa incremental driver (layer 7) — codegen exposes plain query-shaped
  functions; salsa wiring comes later.
- Bundling / multi-file import resolution beyond what `check_program`'s flatten
  already provides.
- Source maps beyond a stub `emit_with_source_map` entry point (can follow).
- A second backend; the HIR is designed to *allow* one, but none is built now.
