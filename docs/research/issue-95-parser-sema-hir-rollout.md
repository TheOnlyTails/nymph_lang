# Issue #95: parser, semantic-interface, and HIR rollout

Status: resolved research plan for [issue #95](https://github.com/TheOnlyTails/nymph_lang/issues/95),
based directly on `language-identity-plan@origin` commit `56f05139c160`.

This is a rollout plan, not an implementation. It combines the committed dependency inventory from
[issue #83](./issue-83-language-identity-dependency-graph.md) with the settled decisions in
[issues #84–#98](https://github.com/TheOnlyTails/nymph_lang/issues/82). The central rule is:

> Evolve stable semantic contracts before their syntax. A source form is accepted only in the same
> mergeable step that can check it, publish complete and recovered interfaces for it, lower it through
> stable body IDs into backend-neutral HIR, and either emit it or reject it with an ordinary diagnostic.

The existing `CompilerSession`, Salsa graph, complete/recovered interface split, per-definition runtime
artifacts, and stable lowering gateway remain the architecture. There is no second checker, frontend
cache, AST lookup from lowering, or long-lived dual language.

## Authority and settled corrections

[`docs/design/language-identity.md`](../design/language-identity.md),
[`docs/design/async-model.md`](../design/async-model.md), and [`CONTEXT.md`](../../CONTEXT.md) state the
intended identity. Later issue resolutions intentionally correct four stale draft passages:

- #91 replaces visibility-filtered `echo` with complete ordinary-value rendering that never dispatches
  `Debug` ([draft passage](../design/language-identity.md#privacy-and-debugging)).
- #97 replaces wrapper/DAG enum embedding and generated `Into` with fixed-point variant sets, erased
  static views, and explicit-only `Into` ([draft passage](../design/language-identity.md#enum-variant-embedding-and-spreading)).
- #98 replaces `Option<#(Item, self)>` iteration and mutation-oriented loops with nominal
  `Iteration<Item, Next>`, dedicated `For`, and immutable state loops
  ([draft passage](../design/language-identity.md#iteration-without-mutation)).
- #92 settles the root rendering and exit policy which the async draft still calls open
  ([draft passage](../design/async-model.md#entrypoints)).

#90 assigns a generated bridge to its destination module, but #97 later chooses not to generate enum
`Into` bridges at all. These are settled draft corrections, not reopened design questions. #99 must
reconcile the governing documents before they become public migration documentation. No infeasible
guarantee, prohibitive cost, or user-owned HITL decision was found.

## Current ownership and integration seams

```text
source
  │
  ▼
nymph-syntax ── recovered AST + parser NodeId ──▶ nymph-sema Checker
                                                      │
                                     resolved CheckedFacts at completion
                                                      │
                           ┌──────────────────────────┴─────────────────────────┐
                           ▼                                                    ▼
                  complete/recovered                                  stable body copy +
                  ModuleEnvironment                                    RuntimeAnnotations
                           │                                                    │
                           └─────────────────────┬──────────────────────────────┘
                                                 ▼
                                      StableLoweringContext
                                                 │
                                                 ▼
                                      backend-neutral HIR
                                                 │
                           ┌─────────────────────┴──────────────────────────────┐
                           ▼                                                    ▼
                    JavaScript emitter                                 host runtime/launcher
```

The current code already has the right boundaries, but its schemas still describe the mutable
language:

- **Parser and AST.** The hand-written recovering parser owns expression IDs
  ([`parser/mod.rs:27-99`](../../crates/nymph-syntax/src/parser/mod.rs#L27-L99)). `NodeId` is parser-order
  identity for expressions only ([`ast/lib.rs:77-87`](../../crates/nymph-ast/src/lib.rs#L77-L87)). The AST
  stores integer literals as `u64`, assignment and `while`, and mutable declarations/views
  ([`expr.rs:156-279`](../../crates/nymph-ast/src/expr.rs#L156-L279),
  [`decl.rs:126-180`](../../crates/nymph-ast/src/decl.rs#L126-L180),
  [`ty.rs:36-52`](../../crates/nymph-ast/src/ty.rs#L36-L52)). Calls already retain names and a spread
  boolean, but need a value/spread sum type so impossible combinations are unrepresentable
  ([`expr.rs:380-385`](../../crates/nymph-ast/src/expr.rs#L380-L385)). `async`/`await` are reserved tokens,
  not accepted syntax ([`token.rs:72-76`](../../crates/nymph-ast/src/token.rs#L72-L76)).
- **Semantic completion.** `Checker` owns one interner, definitions, signatures, solver state, and
  annotations ([`check.rs:53-124`](../../crates/nymph-sema/src/check.rs#L53-L124)). The existing immutable
  completion boundary resolves inference before publishing `CheckedFacts`
  ([`check.rs:476-625`](../../crates/nymph-sema/src/check.rs#L476-L625)); effect solving and mandatory
  range obligations belong immediately before that publication. Semantic callable types currently have
  no effects and still carry `TyKind::Mut`
  ([`ty/mod.rs:26-77`](../../crates/nymph-hir/src/ty/mod.rs#L26-L77)). Coercion currently accepts every
  `uint -> int` and strips mutable views
  ([`coerce.rs:19-95`](../../crates/nymph-sema/src/coerce.rs#L19-L95)).
- **Stable interfaces.** `InterfaceType::Function` has parameters and a return only, while mutable types,
  parameters, fields, members, and impls are fingerprinted
  ([`interface.rs:67-93`](../../crates/nymph-sema/src/interface.rs#L67-L93),
  [`interface.rs:374-495`](../../crates/nymph-sema/src/interface.rs#L374-L495)). Complete and recovered
  interfaces have parallel schemas and structural fingerprints
  ([`interface.rs:619-754`](../../crates/nymph-sema/src/interface.rs#L619-L754)). `ModuleIdentity` carries
  origin/project/path but no exact package instance
  ([`identity.rs:7-21`](../../crates/nymph-sema/src/identity.rs#L7-L21)).
- **Salsa and session.** `CompilerSession` is the sole state owner
  ([`session.rs:385-397`](../../crates/nymph-compiler/src/project/session.rs#L385-L397)); current module inputs
  are `(ProjectId, ModulePath, source)`
  ([`session.rs:120-150`](../../crates/nymph-compiler/src/project/session.rs#L120-L150)). The canonical query
  checks a module from its own recovered tree and dependency environments
  ([`queries.rs:2372-2526`](../../crates/nymph-compiler/src/project/queries.rs#L2372-L2526)), then publishes a
  complete interface only when clean and a recovered interface otherwise
  ([`queries.rs:2563-2643`](../../crates/nymph-compiler/src/project/queries.rs#L2563-L2643)). Stable shape lookup
  correctly refuses recovered facts
  ([`queries.rs:1591-1651`](../../crates/nymph-compiler/src/project/queries.rs#L1591-L1651)).
- **Stable runtime facts.** Parser IDs are remapped to body-local `BodyNodeId` before per-definition
  artifacts are published ([`runtime.rs:1049-1110`](../../crates/nymph-sema/src/runtime.rs#L1049-L1110)).
  `RuntimeAnnotations` is already the required stable side-table boundary
  ([`runtime.rs:394-418`](../../crates/nymph-sema/src/runtime.rs#L394-L418)); it currently records blanket
  `uint -> int` decisions and an Option-based iterator ABI
  ([`runtime.rs:366-435`](../../crates/nymph-sema/src/runtime.rs#L366-L435)). Stable lowering consumes only
  exact identities, stable shapes, names, runtime artifacts, and annotations—never Salsa or a module AST
  ([`stable_lowering.rs:1-105`](../../crates/nymph-sema/src/stable_lowering.rs#L1-L105),
  [`stable_lowering.rs:162-207`](../../crates/nymph-sema/src/stable_lowering.rs#L162-L207)).
- **HIR and JavaScript.** HIR still exposes mutable lets, raw `f64` integer payloads, transitional identity
  equality, stringly external calls, assignment, and `While`
  ([`hir.rs:46-55`](../../crates/nymph-hir/src/hir.rs#L46-L55),
  [`hir.rs:133-212`](../../crates/nymph-hir/src/hir.rs#L133-L212),
  [`hir.rs:266-300`](../../crates/nymph-hir/src/hir.rs#L266-L300),
  [`hir.rs:389-424`](../../crates/nymph-hir/src/hir.rs#L389-L424)). Stable `for` lowering manufactures mutable
  counters and `While` nodes ([`stable_lowering.rs:5910-6079`](../../crates/nymph-sema/src/stable_lowering.rs#L5910-L6079),
  [`stable_lowering.rs:6195-6266`](../../crates/nymph-sema/src/stable_lowering.rs#L6195-L6266)). Codegen directly
  emits assignment and JavaScript `while`/completion packets
  ([`emit.rs:2284-2356`](../../crates/nymph-codegen/src/emit.rs#L2284-L2356),
  [`emit.rs:2893-3054`](../../crates/nymph-codegen/src/emit.rs#L2893-L3054)).
- **Runtime and FFI.** The compiler's host graph is the central owner of embedded Node modules
  ([`host_runtime.rs:1-136`](../../crates/nymph-compiler/src/host_runtime.rs#L1-L136)), while HIR/ABI carry
  static module strings and one marshal kind. `NInt`/`NUint` boxes still hold JavaScript Number, lists
  and maps expose mutation, and equality/hash/debug are universal reflection
  ([`box_rt.rs:51-118`](../../crates/nymph-codegen/src/box_rt.rs#L51-L118)). Stable emission rejects recovered
  environments, links per-definition fragments, and merges host modules before bundling
  ([`emission.rs:130-261`](../../crates/nymph-compiler/src/project/emission.rs#L130-L261),
  [`emission.rs:581-617`](../../crates/nymph-compiler/src/project/emission.rs#L581-L617)).
- **Tooling and compatibility.** The formatter reparses and refuses malformed input, then exhaustively
  walks AST nodes ([`format/lib.rs:49-70`](../../crates/nymph-format/src/lib.rs#L49-L70),
  [`format/lib.rs:410-590`](../../crates/nymph-format/src/lib.rs#L410-L590)). LSP overlays retain the same
  compiler session plus a separate no-prelude session, not an alternate checker
  ([`compiler_state.rs:166-230`](../../crates/nymph-lsp/src/compiler_state.rs#L166-L230)). One-shot APIs are
  adapters over the same project pipeline ([`compiler/lib.rs:1-25`](../../crates/nymph-compiler/src/lib.rs#L1-L25)).
  Keep all three compatibility boundaries.
- **Projects and launch.** `nymph-project` owns manifest/filesystem policy but no compiler identity or
  session ([`project/lib.rs:1-38`](../../crates/nymph-project/src/lib.rs#L1-L38)). Its manifest has a package
  name/version and build entry, but no resolved package identity or profile
  ([`project/lib.rs:19-63`](../../crates/nymph-project/src/lib.rs#L19-L63)). Entry validation currently
  checks AST spelling rather than the resolved return type
  ([`entry.rs:32-70`](../../crates/nymph-sema/src/entry.rs#L32-L70)). `nymph run` keeps ordinary module output
  inert, appends a direct call to emitted `main`, starts Node, and forwards its status
  ([`run.rs:47-60`](../../crates/nymph-cli/src/commands/run.rs#L47-L60),
  [`run.rs:93-123`](../../crates/nymph-cli/src/commands/run.rs#L93-L123)). R12 replaces only that launcher
  policy, not library compilation or the shared project target pipeline.
- **Migration surface.** The stdlib currently defines mutating Option-returning iterators
  ([`iter/mod.nym:5-66`](../../stdlib/src/iter/mod.nym#L5-L66)), mutable list/map interfaces
  ([`list.nym:1-21`](../../stdlib/src/collections/list.nym#L1-L21),
  [`map.nym:1-23`](../../stdlib/src/collections/map.nym#L1-L23)), mutation-oriented host adapters
  ([`list.ts:12-42`](../../stdlib/src/collections/list.ts#L12-L42),
  [`map.ts:14-30`](../../stdlib/src/collections/map.ts#L14-L30)), and blanket equality/hash/debug
  ([`ops/mod.nym:465-484`](../../stdlib/src/ops/mod.nym#L465-L484)). These are migration fixtures until
  cutover, not code to delete early.

## Rollout invariants

Every step below is independently mergeable: it has explicit prerequisites, leaves accepted programs
on one coherent path, and has a focused gate. The implementation may split a step into smaller commits
only where each commit preserves these invariants.

1. **One identity chain.** Parser `NodeId` is local source identity. Semantic/runtime identity is
   `PackageId -> ModuleIdentity -> DefinitionId -> BodyNodeId`. Spans and names never replace an ID.
2. **Two availability products, one schema.** Any fingerprinted complete-interface field is represented
   in recovered form as `Known | Poison` (or an equivalent availability marker) in the same step.
   Recovered interfaces serve diagnostics/tooling; lowering continues to require complete facts.
3. **One semantic completion point.** Type inference, the separate least-effect solver, range obligations,
   enum-set fixed points, and construction/dispatch plans are finalized before `CheckedFacts` publication.
4. **Stable lowering stays semantic-only.** New lowering decisions enter per-definition artifacts and
   `RuntimeAnnotations`. Lowering never asks Salsa, reads another module's AST, redoes a proof, or infers
   behavior from a JavaScript symbol.
5. **HIR names language operations.** It may name checked/direct integer operations, persistent collection
   operations, struct fresh/update, enum views, `For`, state transitions, calls/tail calls/suspension,
   cleanup, external adapters, and echo. It may not expose Promise, `AbortSignal`, trie/HAMT nodes,
   JavaScript stack frames, or source mutability.
6. **Syntax is the last producer.** Tokens may be reserved and AST/HIR data types may land unused. Parser
   acceptance is enabled only after sema, complete/recovered interfaces, stable runtime facts, HIR,
   Node emission/runtime, formatter traversal, and a normal unsupported-case diagnostic exist for that
   node family. An internal synthetic-fact or HIR test is not public syntax acceptance.
7. **No feature-flag language fork.** During migration, old mutable syntax and new immutable syntax may
   coexist briefly, but each syntax has exactly one semantics. Compatibility adapters are compiler- or
   runtime-private and have a named deletion step.

## Dependency-ordered merge sequence

The recommended landing order is R0–R14 below. R11 can proceed after R6 while R7–R10 are underway, but
landing it in the listed order minimizes repeated HIR/runtime rewrites. R14 cannot begin until the #99
tooling/migration gate is complete.

### R0 — Freeze the stable-pipeline contract

**Owns:** `nymph-sema` interface/runtime test support and `nymph-compiler` query tests.

- Snapshot complete and recovered interfaces, their fingerprints, per-definition runtime artifacts,
  stable HIR, canonical emitted names, and one-shot/retained-session equivalence for representative
  current modules.
- Add query counters proving a body-only edit preserves an unchanged interface and unrelated importer
  reuse, while a header edit invalidates the expected importer facts.
- Record that stable emission and stable-shape lookup reject recovered environments. Do not add syntax.

**Merge gate:** interface/fingerprint determinism across repeated sessions; complete/recovered recovery
fixtures; body-only and header-edit invalidation tests; one-shot versus retained-session diagnostics and
emission parity.

### R1 — Install exact package, module, and nominal identity

**Prerequisite:** R0.

**Owns:** `nymph-compiler` mints identities and Salsa inputs; `nymph-project` only parses/resolves
filesystem manifest data; `nymph-sema` serializes identity; stable name/link planning consumes it.

- Add compiler-owned `PackageId` for each exact resolved dependency-graph node. Keep `ProjectId` as the
  session/workspace lifetime. Key project modules conceptually by
  `(ProjectId, PackageId, ModulePath)` and make `ModuleIdentity` carry all three.
- Thread package identity through project/builtin inputs, import resolution, every definition/member/
  implementation ID, complete and recovered interfaces and fingerprints, runtime owners, canonical
  module specifiers, emitted binding names, and host/runtime collision checks.
- Alias imports resolving to one graph node share an ID; independently resolved copies do not.
  Manifestless facades receive isolated synthetic package IDs. Importable `std` and compiler ambient
  modules retain reserved domains.
- Access checks compare package IDs for `internal` and full module identities for `private`; interface
  extraction remains complete and contextual environments decide availability.

**Syntax gate:** none. Import spelling does not expose or manufacture IDs.

**Merge gate:** alias/same-node and same-name/different-node identity tests; source edits preserve IDs;
manifest-resolution edits replace only affected package nodes; complete/recovered fingerprint tests;
three-context visibility; no canonical module-specifier collision; one-shot synthetic-package parity.

### R2 — Add canonical effects and the stable external-ABI vocabulary

**Prerequisite:** R1.

**Owns:** `nymph-sema` owns nominal effect IDs, rigid effect parameters, the separate subset solver, and
call charging; semantic callable types carry resolved rows; module interfaces fingerprint rows and ABI
plans; HIR carries selected semantic operations but no runtime effect set.

- Extend callable semantic types, signatures, constraints, interface/default/impl members, task and
  iterator latent types, `Close<!E>`, and external declarations with canonical finite rows. Canonicalize
  by stable nominal effect ID, not spelling or insertion order.
- Run a separate least-solution subset solver after ordinary type inference. Closed annotations are
  upper bounds; inferred remainders find a least fixed point; concrete implementations may narrow an
  interface contract. Resolve every exported row before the `CheckedFacts` completion boundary.
- Extend complete and recovered interfaces, headers, fingerprints, runtime artifacts, and stable-shape
  requests in the same change. Body-only inferred implementation changes must not affect callers unless
  an exported resolved row changes.
- Replace the current single-marshal `ExternalAbi` schema with the backend-neutral fields settled in
  #94: logical adapter ID, canonical effect row, external-state audit metadata, transaction behavior,
  ordinary versus cancellable call mode, and parameter/result marshal plans. R9 will install the Node
  delivery.

**Syntax gate:** accept `effect`, `!()`, nominal rows, `!E`, `!_`, effect generics, and effectful callable
types only when parser/AST, formatter traversal, complete/recovered interfaces, solver diagnostics, and
stable runtime facts all land. Effects are erased at ordinary JS emission; unsupported async/resource
uses remain ordinary semantic errors until R8/R9 rather than half-lowered nodes.

**Merge gate:** row ordering/idempotence; recursive least solutions; explicit-bound violations; narrower
impls; generic/interface versus concrete call charging; complete/recovered round trips and fingerprint
invalidation; task/iterator/cleanup type fixtures; FFI metadata fingerprints; no runtime effect payload.

### R3 — Make integers exact and publish range-proof decisions

**Prerequisite:** R0; R2's effect work is independent, but landing R3 afterward avoids two interface/HIR
schema migrations at once.

**Owns:** lexer/AST retain exact fixed-width literal magnitude, sema constant folding and range analysis
use arbitrary-precision compiler integers, `RuntimeAnnotations` carries auditable obligation decisions,
HIR selects checked versus proven-safe operations, and the Node runtime boxes in-range BigInt.

- Remove `f64` from integer HIR and emitted integer constants. Represent `int`/`uint` exactly through
  parsing, semantic constants, interface defaults, stable bodies, folding, patterns, HIR, JS literals,
  hashing, and FFI. Floats remain `f64`.
- Add the body-local interval/exclusion/signed-pair-bound analysis from #93 to the canonical module
  analysis. Resolve arithmetic, conversions, indexing, host indices, and slicing as invalid, proven
  safe, or unknown before semantic publication.
- Store compact proof decisions and evidence keyed by `BodyNodeId`; HIR carries an operation plus
  `Checked`/`Direct` mode, never the full abstract state. Imported body facts cannot change source
  acceptance or semantic fingerprints.
- Replace unconditional `uint -> int` with proof-directed conversion. During the compatibility window,
  an old unproved implicit conversion lowers to an explicit checked legacy operation and records a
  legacy-use marker. #99 selects the diagnostic timing and policy; R14 removes acceptance. New APIs must
  not introduce another blanket widening.

**Syntax gate:** existing literal/range syntax remains accepted through the new exact path. New
conversion APIs become callable only with complete signatures, proof annotations, HIR modes, BigInt
runtime helpers, and FFI plans.

**Merge gate:** signed/unsigned min/max, overflow, division-by-zero, invalid-shift, cast, negative-index,
exclusive/inclusive/reversed-slice matrices; constant folding; proof replay; checked/direct HIR snapshots;
BigInt JS and FFI; caller diagnostics and interface fingerprints unchanged after imported body-only edits.

### R4 — Replace observable collection mutation and universal reflection

**Prerequisite:** R3.

**Owns:** HIR names persistent operations; the canonical boxed runtime owns the 32-way vector trie,
trimmed slices, HAMT, set wrapper, private transients, and deterministic hash cache; sema owns lawful
equality/hash capability selection and nominal derives.

- Add semantic HIR operations for list construction/read/append/replace/slice and map/set construction/
  read/insert/remove. Do not expose trie nodes, native `Map`, or mutable builder handles.
- Change `NInt`/`NUint` payloads and the structural hash to exact BigInt, with two internal 32-bit lanes
  and signed 64-bit public output. Domain-separate nominal/kind identity; use ordered combination for
  lists/tuples/fields and commutative cardinality-aware combination for maps/sets.
- Replace blanket reflective equality/hash with static capability selection and complete generated
  nominal implementations. Hidden fields participate. Functions, unlawful float equality, and opaque
  externals have no automatic lawful path. Remove `IdentityBoolean` as soon as all equality sites use
  the selected protocol.
- Temporary old `impl mut` collection calls route through explicitly legacy HIR/runtime operations so
  the current stdlib and migration corpus continue to build. New immutable APIs use only persistent
  operations; the legacy adapters are deleted in R14.

**Syntax gate:** collection literal syntax keeps one meaning and starts producing persistent values when
this step lands. Immutable update/conversion APIs may be exposed now. Source assignment and old mutating
methods remain legacy-only until R14; no new syntax is needed.

**Merge gate:** old-value alias preservation; vector/HAMT structural sharing and collision nodes;
trimmed-slice retention; unordered insertion permutations; hidden fields; enum-normalization hook;
`equal => same hash`, including nonnegative cross-type integer keys; hash-cache soundness; static rejection
of unlawful keys; Node runtime and FFI snapshot tests.

### R5 — Land immutable structs as one vertical slice

**Prerequisites:** R1, R2, R4.

**Owns:** AST calls keep unresolved callee syntax but use explicit `Value`/`Spread` arguments; sema owns
`StructConstructionPlan`; interfaces own complete field shape; nominal-owner runtime artifacts own
defaults; HIR distinguishes fresh construction from clone/update.

- Normalize field visibility and retain field ID, order, type, visibility, and `has_default` in complete
  and recovered interfaces. Contextual availability, not destructive filtering, controls access.
- Record plans keyed by struct/field IDs: `Fresh` with ordered supplied/default fields or
  `CloneUpdate` with one exact source and ordered replacements. Default bodies remain separate artifacts
  owned and lowered in the declaring module.
- Add `StructFresh` and `StructCloneUpdate` HIR. JavaScript evaluates supplied/source/replacement values
  once left-to-right, preserves hidden fields, runs defaults only for fresh construction in declaration
  order, and never mutates the source.

**Syntax gate:** accept named fresh construction, one first source spread, clone/update, named patterns,
and required anonymous pattern `...` only with the full vertical slice. Positional constructors/patterns
may remain accepted solely as tagged legacy forms until R14; they never enter the new plans.

**Merge gate:** parser/formatter round trips; malformed spread/count/order; complete/recovered shape and
fingerprint changes; every public/internal/private context; package/module identity; exact generic source;
duplicate/unknown/missing fields; default ownership/evaluation trace; hidden-field clone preservation;
pattern omission; HIR snapshots and Node old-value tests.

### R6 — Normalize enums to variant sets and erased static views

**Prerequisites:** R1, R2, R4, R5.

**Owns:** sema owns source-variant identities, fixed-point set expansion, assignability, coherence,
dispatch, and deep `?`; stable interfaces fingerprint sorted sets and generic projections; HIR owns enum
view operations and selected targets; JS owns only source-variant values.

- Give every source variant a nameable single-variant type identity: source variant `DefinitionId` plus
  only generic arguments used by its fields. Compute each enum's canonical deduplicated set by least
  fixed point; cycles, self-edges, repeated paths, and diamonds converge rather than diagnose.
- Add an SCC-level, body-independent enum-header/fixed-point query in the existing Salsa graph for
  module-spanning cycles. It consumes stable headers and publishes set facts before full body analysis;
  it must not recursively request complete interfaces through the cycle.
- Serialize final sorted sets, source identities, and generic projections in complete/recovered
  interfaces and fingerprints. Use set inclusion for assignment/arguments/returns/casts and static-view
  method dispatch. Qualified patterns restore source view; unrefined values retain destination view.
- HIR names view change/refinement, canonical source construction, variant match, and selected dispatch.
  JavaScript erases the view and emits each variant only in its source module.
- Equality is legal for overlapping sets and compares source identity/fields. Deep `?` first uses direct
  set assignability; otherwise it requires one unique pure, infallible explicit `Into`. Direct inclusion
  wins. Generate no enum `Into`.

**Syntax gate:** accept whole/selected embedding declarations, static-view casts, source-qualified
patterns, and deep propagation only after the SCC query, interfaces, HIR, dispatch, equality/hash, and JS
erasure are complete. Removed wrapper construction and spread-pattern spellings remain legacy diagnostic
inputs only.

**Merge gate:** self/cycle/diamond/repetition fixed points; cross-module SCC invalidation; generic
projection identity (`None` sharing versus field-bearing variants); inclusion and uncovered casts;
overlap equality/hash; concrete and generic static-view dispatch; reachability/exhaustiveness; HIR has no
wrapper allocation; Node identity; direct, missing, ambiguous, effectful, and fallible deep-`?` cases.

### R7 — Move every generated Nymph callable to one activation ABI

**Prerequisites:** R2–R6.

**Owns:** stable lowering marks ordinary versus tail calls and lexical transfers; HIR names activations,
calls, tail transfers, suspension points, returns, and cleanup-region transitions; the host runtime owns
the activation driver. External adapter calls remain outside this ABI.

- Represent each generated callable as defunctionalized states with explicit resume state, live locals,
  lexical cleanup scopes, and the current execution-frame slot. Hidden generic type objects remain
  ordinary trailing calling-convention arguments.
- Push for non-tail calls, retain/resume for suspension, and replace for tail calls. A tail transfer first
  unwinds scopes exited by the current activation; a cleanup defect prevents the destination.
- Route return, `?`, panic, cancellation, normal scope exit, and later `let use` through this one unwind
  protocol. Do not retain a second JavaScript `finally` or native async unwind.
- Migrate all current generated Nymph callables in this step. A direct-JS emitter may exist only in
  isolated comparison tests, not as a selectable production semantics.

**Syntax gate:** no async or managed-resource syntax yet. Existing callable syntax changes representation
only; proper tail calls become guaranteed when the activation path passes the gate.

**Merge gate:** 100,000 direct/mutual and deep generic/dynamic/branch/match tail transfers with one logical
activation; non-tail push/resume; hidden type arguments; return/`?` control; nested reverse cleanup using
synthetic cleanup HIR; cleanup-defect aggregation; source-map activation names; external calls still use
adapter HIR and never the activation ABI.

### R8 — Add cold tasks and explicit execution frames

**Prerequisites:** R2, R7.

**Owns:** sema owns task/handle types, async legality, and effect charging; HIR owns recipe/drive/spawn/
observe/context/suspension operations; generated callables receive one hidden frame; host runtime owns
executions, outcomes, contexts, cancellation, selection/racing, and joins.

- A generated task is a cold recipe closure plus one memoized default handle. Direct driving starts or
  observes that handle; explicit spawn creates a fresh execution; observing a running handle is pure.
- The frame carries inherited structured context, cancellation lineage, and host cancellation slot.
  Async functions inherit context; async blocks replace only structured context with a nested join scope.
- Implement unsuppressible cooperative cancellation at suspension/checkpoints, child cancellation/join,
  deterministic select, owning race, loser cleanup, and primary/suppressed defect preservation.
- Promise and `AbortController` remain host-runtime mechanisms. Neither enters semantic types or HIR.

**Syntax gate:** turn the existing reserved `async`/`await` tokens into accepted `AsyncFunction`,
`AsyncBlock`, and `Await` AST nodes only when effects, complete/recovered callable interfaces,
`RuntimeAnnotations`, task HIR, activation suspension, host kernel, formatter/LSP traversal, and ordinary
diagnostics all land.

**Merge gate:** cold construction; memoized default versus fresh spawn; inherited versus nested contexts;
handle-result nesting; child defect observation; deterministic already-settled and first-settlement
selection; race loser cancellation/join and cleanup defects; explicit checkpoints; no implicit suspension;
constant logical stack across suspend/resume/tail cycles.

### R9 — Complete managed cleanup and the Node FFI adapter boundary

**Prerequisites:** R1–R3, R7, R8.

**Owns:** sema recognizes any nominal `Close<!E>` and verifies lexical cleanup/effects; HIR owns cleanup
calls and backend-neutral adapter calls; compiler lowering owns marshalling; the Node host graph owns
logical-adapter delivery and `AbortSignal` plumbing.

- Add `let use` as its own binding kind and record activation-owned cleanup regions keyed by stable body
  IDs. Aliases and repeated close are legal; implementation-owned shared closed state and declared
  post-close errors remain ordinary behavior. There is no ownership checker or universal wrapper.
- Charge direct and generated cleanup from the same resolved `Close<!E>` row. Close is synchronous,
  returns `void` rather than a declared `Result`, is idempotent, and gets no `AbortSignal`. Attempt every
  close in reverse order and preserve primary/suppressed defects.
- Add nominal opaque external types and lower all external calls from the stable ABI plan introduced in
  R2. Compiler marshalling owns scalars/composites/opaque references; exact integers cross as BigInt.
- Resolve logical adapter IDs through the Node registry. Ordinary calls are args-only. Cancellable calls
  alone receive the frame's signal as one hidden trailing argument. Trusted ABI violations defect; they
  are never repaired or converted into declared `Result`.

**Syntax gate:** accept `let use`, external type declarations, external effect/audit/cancellation metadata,
and managed cleanup only after all exits lower through the activation machine and every logical adapter
has validated Node delivery. No partial parser-only resource form.

**Merge gate:** normal/return/`?`/tail/panic/cancellation cleanup; multiple resources/defects; escaped alias
and repeated close; effect charging; child-capture warning facts; ordinary versus cancellable adapter ABI;
missing/mismatched adapter; BigInt and opaque-reference marshalling; post-close declared errors; direct
versus spawned trusted-FFI defects.

### R10 — Switch iteration, `For`, and immutable state loops together

**Prerequisites:** R2–R4, R7–R9.

**Owns:** sema owns `Iteration<Item, Next>`, capability-preserving iterator types, latent rows, exact-size
facts, and control contracts; HIR owns `For` and state-loop transitions; activation/runtime code owns
cursor slots, cleanup, cancellation exits, and optional private fusion; stdlib owns adapters/terminals.

- Replace Option-returning mutable `next` with persistent nominal `Done | Yield(item, next)` and preserve
  the receiver's full static capability set through `self`. Add `ExactSizeIterator.remaining()` and the
  settled propagation/loss rules.
- Make iterator/adaptor creation pure. Charge source and callback rows at sequential consumption; require
  algorithm-scheduled callbacks such as sorting comparators to be pure. Keep one public iterator model.
- Replace `RuntimeIteration`'s Option ABI and mutable `lower_for` desugaring with dedicated HIR `For`
  carrying dispatch, effects, successor state, control targets, and source site. Save the successor before
  the body; abandon it on every exit without another step.
- Add HIR `StateLoop` with immutable header slots and simultaneous named `ContinueTransition` updates.
  The activation machine orders acquisition, body-local cleanup, replaced-header reverse cleanup,
  installation, and next iteration exactly as #98 specifies.
- Atomically migrate stdlib adapters/terminals and collection aliases to persistent results, generic
  `.to<T>()`, named aliases, last-wins map collection, exact size, and lazy sorting. Runtime-private
  transients/fusion must freeze before observation and preserve order/effect/exit behavior.

**Syntax gate:** existing `for` switches to successor-state semantics only with the stdlib/runtime cutover.
Accept `loop` headers and argument-bearing named `continue` only when state-loop HIR and cleanup transitions
are complete. Keep `while` as a legacy accepted form until R14, but do not lower new destination features
through it.

**Merge gate:** replay and branching; repeated effectful-state consumption count/order; adapter and
short-circuit order; exact-size propagation/loss; lazy-sort first pull; duplicate keys; every `for` exit;
labelled control; no extra `next`; simultaneous old-binding evaluation; managed header replacement and
failure cleanup; closure capture; deep constant-stack state loops; private fusion equivalence; stable HIR
contains `For`/`StateLoop`, not generated mutable lets/assignment/`While`.

### R11 — Add privileged echo and compiler-owned profiles

**Prerequisites:** R1, R2, R4–R6. It may run in parallel with R7–R10 after those prerequisites.

**Owns:** compiler session/project inputs own `BuildProfile` and `LintLevel`; sema owns operand type/effect
identity and root-package lint selection; HIR owns `Echo(value, site)`; the runtime observer owns inert
complete rendering; frontends only select profile/policy and source URI.

- Add profile/lint values as incremental inputs, development by default. `nymph-project` parses manifest
  policy but does not reinterpret it. Release linting applies only to exact root/workspace `PackageId`;
  dependency/std/compiler echoes erase silently.
- Record one dedicated AST/checked/HIR node. Evaluate once and return the identical value. Development
  calls an observer with basename/line/column and optional frontend URI; release lowers to the operand
  with no observer/helper/site/URI demand.
- Render complete ordinary structure independent of visibility and `Debug`. Functions, resources, and
  opaque externals are inert placeholders. Never invoke getters, proxies, `toString`, user code, or host
  callbacks; rendering failure yields a placeholder without control-flow change.

**Syntax gate:** accept `echo` only with profile inputs, package-aware lint, complete-structure metadata,
HIR erasure/observation, formatter/LSP traversal, and source-site support.

**Merge gate:** exactly-once identity and unchanged effect row; nested complete hidden fields; no `Debug`
dispatch; opaque placeholders/no callbacks; atomic development line and redirected output; virtual sites;
OSC 8 only for interactive stderr; release allow/warn/deny; no release URI/helper bytes; dependency versus
root-package linting; incremental profile invalidation.

### R12 — Validate semantic root shapes and install a Node launcher

**Prerequisites:** R1, R2, R6, R8, R9.

**Owns:** sema validates the resolved, alias-normalized root type and `Display` bound; ordinary build emits
an inert module; the Node launcher owns root context, signals, rendering, and process status.

- Replace AST-spelling checks in `entry.rs` with resolved semantic validation for exactly `void`,
  `Option<void>`, `Result<void,E: Display>`, and the three task-shaped counterparts.
- Keep stable project compilation's emitted module inert. `nymph run` or an explicitly runnable artifact
  adds a separate launcher that creates the root task context, drives a task default execution, joins,
  then classifies only declared Option/Result values.
- Implement the #92 matrix: success 0/silent; expected None/Error 1; cooperative SIGINT 130; SIGTERM 143;
  defect 101 with runtime-owned rendering. A second termination signal may force exit.

**Syntax gate:** no new root syntax. Expanded `main` shapes become accepted only with semantic validation,
launcher emission, task driving, `Display`, signal, and defect handling. Library and one-shot library modes
remain unaffected.

**Merge gate:** every sync/task root shape and invalid near miss; aliases and `E: Display`; inert build
import; exact stderr/newline/status matrix; nested application `Result` remains data; Display defect;
unsourced/signal cancellation; cleanup defect during cancellation; first/second signal behavior; stdout
untouched.

### R13 — Satisfy the #99 diagnostics/tooling/migration gate

**Prerequisites:** R2–R12. #99 owns the detailed rollout plan; this step states what must be true before
R14, not a new execution-ticket decomposition.

- Every destination node has parser recovery, formatter, semantic tokens, completion/hover/definition/
  rename behavior where applicable, and cancellable LSP diagnostics through the existing session.
- Diagnostics are emitted by the owning parser/sema phase with stable codes and causal spans; codegen does
  not discover unsupported accepted syntax. Unambiguous rewrites are machine-applicable.
- Stdlib, examples, executable docs/reference snippets, extension grammar, and test fixtures are migrated
  to persistent collections/iterators, named struct forms, enum views, state loops, exact conversions,
  profiles, and new root shapes.
- Governing docs incorporate the settled #91/#92/#97/#98 corrections listed above. A migration guide maps
  each old form to shadowing, folds, persistent update, state-loop transition, or an explicit host resource.
- Legacy corpus tests stay until R14 proves each old spelling gets its intended targeted error. They are
  not silently reformatted into valid new code.

**Merge gate:** full compiler tests plus malformed recovery; formatter idempotence; LSP feature/wire tests;
extension compile/tests; docs build and executable fences; all examples; release echo/root matrices; a
machine-readable inventory showing no destination source still uses mutable forms.

### R14 — Perform one coherent immutable public cutover

**Prerequisite:** R13 and all prior runtime/stdlib migrations.

This is the only step that rejects the old mutable language globally. It must not be split into a state
where valid source reaches a deleted checker or codegen branch. The exact deletion sequence is in the
next section.

**Merge gate:** targeted migration diagnostics for every removed spelling; zero mutable forms in accepted
stdlib/examples/docs; no legacy AST/sema/stable-body/HIR/codegen/runtime operation reachable; complete and
recovered interface fixtures contain no mutable schema; full Rust/Node/docs/extension suites; repository
searches for the retired symbols; old programs fail with diagnostics rather than panic.

## Temporary compatibility and retirement ledger

| Scaffolding                                          | Purpose                                                                                                  | Retirement                                                                                                                                         |
| ---------------------------------------------------- | -------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| Legacy mutable AST/sema/stable-body/HIR/codegen path | Build the current stdlib and retain migration fixtures while replacements land                           | Stop producing and delete in R14                                                                                                                   |
| Legacy mutating collection adapters                  | Keep old `impl mut` APIs working while persistent operations and stdlib replacements land                | R14, after stdlib/examples migrate                                                                                                                 |
| Unproved `uint -> int` checked bridge                | Preserve a bounded migration window without Number truncation                                            | R14; unproved implicit conversion becomes an error                                                                                                 |
| Current external ABI to full adapter-plan projection | Let existing linked externals operate while fingerprinted ABI fields land in R2                          | R9 after every Node adapter validates against the full plan                                                                                        |
| Number/BigInt host bridge                            | Permit focused runtime/adapter migration without a public mixed integer ABI                              | End of R3 for compiler output; remaining host tests/adapters by R9                                                                                 |
| Option-based iterator ABI and mutable `lower_for`    | Keep current `for` working until activation cleanup and persistent stdlib are ready                      | Replaced atomically in R10                                                                                                                         |
| Current `std/option` re-export shim                  | Make host modules share canonical Option until exact identity/name routing can import its owner directly | Remove after R1/R4 host linking uses the exact canonical owner; do not couple it to iterator removal                                               |
| Transactional REPL cells/rollback hooks              | Existing implementation detail for submission rollback                                                   | In R14 remove source-assignment-specific cells; retain only transaction/version and runtime-private mutation needed by activation state/transients |

The following are **permanent compatibility boundaries**, not scaffolding: complete/recovered interfaces;
`RuntimeAnnotations` keyed by `BodyNodeId`; stable lowering lookup traits; `ModuleAnnotations`' compatibility
wrapper; one-shot compiler facades over `CompilerSession`; LSP's retained normal/no-prelude sessions;
manifestless synthetic packages; runtime-private mutation for activation state, transients, caches, and host
resources. Ordinary-value immutability does not require deleting unobservable runtime mutation.

## Exact superseded-path deletion order

R14 follows this producer-to-consumer order. Closely coupled Rust enum and exhaustive-match edits may be
one commit, but the logical order and reachability assertions remain:

1. **Migrate all source producers.** Convert embedded stdlib, examples, docs, tests, CLI synthesis, and
   extension fixtures. Remove stdlib `impl mut`, mutating iterator/collection signatures, assignments,
   and `while`. Keep a frozen legacy-input corpus outside accepted sources.
2. **Turn acceptance into recovery-only diagnostics.** Parser branches consume `let mut`, `mut` params,
   `mut func`, `mut T`, assignments/compound assignments, positional struct forms, wrapper enum forms,
   and `while`, emit the #99 diagnostic, and synthesize poison/immutable recovery nodes that cannot be
   lowered. Tokens may remain solely to recognize these errors.
3. **Delete mutable AST shapes.** Remove `LetKind::Mut`, `FuncKind::Mut`, `FuncParam.mutable`, `Type::Mut`,
   `ExprKind::AssignOp`, and `ExprKind::While`; make call arguments `Value | Spread`; update every parser,
   formatter, LSP, docs, and test walker. Parser recovery from step 2 must no longer construct the deleted
   variants.
4. **Delete semantic mutability and legacy conversion acceptance.** Remove checker `Binding.mutable`,
   mutable imports/assignment permissions and assignment inference; `TyKind::Mut` and strip-mut coercion;
   unconditional `uint -> int`; mutable parameter/field/impl flags; `InterfaceType::Mutable`; mutating
   `MemberKind`s; and their complete/recovered `HeaderType`, definitions, impls, and fingerprints.
5. **Delete stable-body/runtime legacy facts.** Remove mutable stable parameters/statements,
   `StableExprKind::AssignOp`/`While`, `implicit_uint_to_int`, old assignment/operator resolutions, and any
   residual Option-based `RuntimeIteration`. Runtime extraction must publish only destination plans.
6. **Delete old stable lowering.** Remove assignment/compound-assignment lowering, mutable place capture,
   `While`, old range/iterator counter desugarings, and legacy collection/external projections. Assert
   stable HIR contains only persistent, `For`, state-loop, activation, cleanup, and adapter operations.
7. **Delete HIR and emitter consumers together.** Remove HIR mutable-let flags, `Assign`, `While`, the raw
   Number loop-counter path, and any remaining identity-equality path; remove their JS emitter branches,
   completion packets, direct mutable declarations, and transactional assignment helper use. Preserve
   explicit backend-private host indices/marshalling rather than mislabeling them as Nymph integers.
8. **Delete superseded runtime/stdlib adapters.** Remove mutating list/map/iterator exports, reflective
   blanket equality/hash/debug, Number integer helpers, old iterator Option bridge, and source-cell
   assignment hooks. Keep trie/HAMT transient builders, activation slots, cancellation state, hash caches,
   and host-resource mutation private and unnameable from Nymph.
9. **Retire migration-only token branches last.** Only after the support window chosen by #99 may lexer/
   parser recognition dedicated solely to old diagnostics be removed. Keeping a token for a permanent
   tailored error is also valid; it is not semantic acceptance or a dual path.

## Explicit handoff prerequisites for issue #99

#99 is unblocked by this plan and should treat the following as fixed input:

1. **Node inventory:** the source families and syntax gates in R2, R5, R6, R8–R11, plus the removed forms
   in R14. Tooling must use the same AST/recovered analysis rather than token-only semantic guesses.
2. **Identity vocabulary:** `ProjectId` is session lifetime; `PackageId` is exact resolved package;
   `ModuleIdentity` includes both plus path; definitions/variants/fields/effects/adapters are stable nominal
   IDs; `BodyNodeId` is per-definition. Diagnostics and links must not print unstable internal indices.
3. **Stable availability:** complete/recovered interfaces share all effect, field, enum-set, ABI, and root
   facts. Tooling can inspect recovered `Known | Poison`; lowering cannot.
4. **Migration mapping:**
   - reassignment/compound assignment → immutable shadowing, persistent collection/struct update, fold, or
     named state-loop `continue`;
   - `while` → `loop` header state and simultaneous transitions;
   - mutating `next(): Option<T>` → `Iteration<T, self>` pattern with successor;
   - positional struct construction/pattern → named fields and explicit omission `...`;
   - enum wrappers/spread patterns/generated conversions → static view inclusion, qualified source
     patterns, or explicit `Into`;
   - unproved integer widening → proven conversion, checked conversion, trapping `as`, or wrapping form;
   - ordinary mutable wrappers → persistent values; live host mutation → explicit opaque external type and
     effects;
   - `mut func` → an ordinary function returning evolved values, or an effectful external operation.
5. **Diagnostic ownership:** parser diagnoses removed grammar; sema diagnoses identity/visibility, row
   constraints, range proof, set coherence, construction, cleanup, and root shapes; runtime defects remain
   runtime outcomes. Codegen must never be the first rejection point.
6. **Tooling gate:** formatter and all exhaustive AST walkers change in the same vertical step as parser
   acceptance; the retained LSP sessions remain the only analysis cache; release diagnostics are selected
   through compiler-owned profile inputs.
7. **Documentation corrections:** #91 complete echo, #92 launcher policy, #97 fixed-point static enum views,
   #98 nominal iteration/state loops, and #84 effect-parameterized cleanup supersede the stale draft text
   identified above.
8. **Cutover contract:** new and legacy syntax may overlap only through R13. R14 removes all valid legacy
   semantics in the order above; no execution ticket may schedule an earlier parser rejection or a later
   backend cleanup.

Issue #100 can decompose execution work only after #99 adds its diagnostics/tooling/documentation gates to
these semantic and HIR boundaries.
