# Issue 83: language-identity implementation dependency graph

Status: planning research, 2026-08-16. `language-identity.md`, `async-model.md`, and
`CONTEXT.md` are the governing design inputs; this note describes the implementation gap, not a
different design.

## Baseline: what is implemented now

- The canonical pipeline is parse → check → lower → emit; errors stop lowering, while the retained
  `CompilerSession` is shared by one-shot and project clients (`crates/nymph-compiler/src/lib.rs:1-25`,
  `68-117`). LSP already retains normal and no-prelude sessions and keys documents by project/module,
  not text/version (`crates/nymph-lsp/src/compiler_state.rs:20-45`, `78-97`, `166-231`). **Reuse** this
  session/query boundary; deepen semantic artifacts rather than add a second tooling checker.
- Syntax/AST currently represents reassignment, `let mut`, `mut func`, mutable parameters, `while`,
  and assignment expressions (`crates/nymph-ast/src/decl.rs:126-180`;
  `crates/nymph-ast/src/expr.rs:479-505`, `616-629`). `async`/`await` are lexed but have no AST expression
  or declaration representation (`crates/nymph-syntax/src/lexer.rs:364-406`;
  `crates/nymph-ast/src/expr.rs:400-523`). Effects, `effect`, `let use`, and `echo` have no token/AST
  model. **Replace** mutable ordinary-language forms; **deepen** the parser/AST for effects, resources,
  async and echo.
- Calls already carry explicit generic arguments and named value arguments
  (`crates/nymph-ast/src/expr.rs:426-430`, `624-629`); the checker instantiates declared generic slots
  and records inferred call arguments (`crates/nymph-sema/src/infer_expr.rs:1505-1539`). **Deepen** this
  seam for `_`, named type arguments and effect binders rather than inventing another call form.
- Types are HM-like with nominal ADTs/interfaces, mutable wrappers, function parameters/return, and
  no effect row (unification walks list/map/tuple/function/ADT/`Mut` only:
  `crates/nymph-sema/src/coerce.rs:99-167`). Today every `uint` is accepted as `int`, explicitly contrary
  to the design (`crates/nymph-sema/src/coerce.rs:19-29`, `42-55`). **Deepen** the type interner and
  callable signatures with canonical effect rows; **replace** unconditional widening with range facts.
- Struct fields carry visibility/default metadata, but constructor checking only matches labels or
  positions and types; it does not enforce field visibility, missing fields, defaults, or source spread
  (`crates/nymph-ast/src/decl.rs:183-189`;
  `crates/nymph-sema/src/infer_expr.rs:2062-2083`, `2137-2184`). Generic list/map spreads exist, but no
  dedicated struct-update or enum-embedding AST exists (`crates/nymph-ast/src/expr.rs:588-598`,
  `638-720`). **Deepen** field provenance/visibility; add explicit spread/embedding nodes.
- HIR is executable and mostly type-erased. It has classes/enums, mutable lets/assignment/while,
  ordinary JS calls, external calls, and no effects, cleanup scopes, tail-call form, task/await form, or
  enum embedding (`crates/nymph-hir/src/hir.rs:10-16`, `46-112`, `133-151`, `228-453`). **Deepen** HIR
  with explicit semantic operations before codegen; do not encode these only as JS strings.
- Uniform boxes and canonical type objects are real foundations: all scalar/list/tuple/map wrappers
  carry `.v`, structural protocol methods, and global tags (`crates/nymph-codegen/src/box_rt.rs:1-27`,
  `51-117`). Project emission imports one `std/box`; standalone emission embeds it
  (`crates/nymph-codegen/src/emit.rs:620-639`). Canonical nominal runtime references/imports are walked
  from HIR (`crates/nymph-hir/src/hir.rs:18-43`). **Reuse/deepen** these ADR-0001/0002 seams.
- Equality/hash are runtime-structural today, not yet lawful static capabilities. Boxes expose
  `equals/hash`, JS externals forward to runtime protocols (`crates/nymph-codegen/src/box_rt.rs:73-80`;
  `stdlib/src/ops/equality.ts:1-9`; `stdlib/src/hash.ts:1-3`), while HIR still labels nonprimitive
  equality as transitional identity (`crates/nymph-hir/src/hir.rs:195-210`). Runtime debug enumerates
  every object key, so it is not source-visibility-sensitive (`crates/nymph-codegen/src/hashmap_runtime.js:176-203`).
  **Deepen** static `PartialEq`/lawful `Eq`/`Hash` selection and replace context-free debug emission.
- Collections and iterators are observably mutable. List/map APIs have `mut` impls and mutate native
  payloads (`stdlib/src/collections/list.nym:4-21`; `stdlib/src/collections/list.ts:12-42`;
  `stdlib/src/collections/map.nym:6-23`); iterator `next` mutates `this`, and adapters/terminals use
  mutable fields/locals/while (`stdlib/src/iter/mod.nym:5-66`, `68-155`). Range `Step` is bounded by JS's
  safe integer 2^53−1, not exact 64-bit (`stdlib/src/range/mod.nym:15-40`). **Replace** these public
  protocols and implementations, retaining linked-external/canonical-Option ABI seams.
- FFI already has checked linkage metadata and explicit marshalling kinds in HIR
  (`crates/nymph-hir/src/hir.rs:214-226`, `266-300`), and codegen deduplicates imports while flagging
  unaudited stateful externals (`crates/nymph-codegen/src/emit.rs:510-525`, `592-608`). **Deepen** ABI
  types with opaque external types and declared effects; keep the trusted boundary and no automatic
  exception/Promise/shape repair.
- Diagnostics already support primary spans, secondary labels, notes, help, severities and stable
  codes (`crates/nymph-diagnostics/src/lib.rs:12-47`, `104-145`) and render source diagrams
  (`crates/nymph-diagnostics/src/lib.rs:164-205`). **Reuse**, but deepen to multi-module related spans
  and machine-applicable edits for effect/resource/async causality.
- Formatter and semantic tokens are AST/compiler consumers, not independent parsers
  (`crates/nymph-format/src/lib.rs:1-16`, `49-70`;
  `crates/nymph-lsp/src/semantic_tokens.rs:1-47`). TextMate still hard-codes old `mut`/`while` and merely
  highlights `async`/`await` (`extension/syntaxes/nymph.tmLanguage.json:103-140`, `170-220`). Docs reuse
  that grammar and compile `nym` fences (`docs/.vitepress/config.ts:1-28`); corpus formatting covers
  stdlib/examples (`crates/nymph-format/tests/corpus.rs:6-39`). **Reuse** integration seams, update only
  after canonical syntax lands.
- Existing end-to-end tests compile and execute emitted JS under Node, but their pinned corpus openly
  excludes `?` and ranges in general value position and validates mutation/while
  (`crates/nymph-compiler/tests/golden_programs.rs:1-22`, `47-99`, `106-139`). These are cutover fixtures,
  not evidence for the new semantics.

## Dependency graph and independently valid migration boundaries

Arrows mean “must stabilize before”. Every numbered boundary is independently mergeable and must
leave the repository green.

```text
A semantic vocabulary/IR
  ├─> B immutable bindings + persistent runtime values ─> E iterators/loops ─┐
  ├─> C effect rows/generics + FFI audit ────────────────> H tasks ──────────┤
  ├─> D lawful equality/hash ─> persistent map/set ──────────────────────────┤
  ├─> F nominal visibility/update ─> G enum embedding/Into/? ────────────────┤
  └─> I exact numerics/range/index ──────────────────────────────────────────┤
A + callable ABI ─> J tail-position lowering/prototype                      │
B + C + H ─> K resources/cleanup/cancellation ─> J cleanup-aware completion │
 all semantic/runtime nodes ─> L diagnostics/session/LSP                    │
 L ─> M formatter/highlighting/docs/examples/tests ─> N public cutover ─────┘
```

1. **A — representation contract.** Add effect-row/capability/opaque-external/task/resource concepts
   to semantic types and stable exported interfaces, plus typed HIR operations, without accepting new
   source syntax. Gate: sema unit tests for canonical idempotent/commutative rows, substitution,
   interface fingerprints and Salsa invalidation; HIR exhaustiveness tests. This prevents syntax from
   outrunning the incremental/public interface model.
2. **B — immutable ordinary core.** Introduce persistent list/map/set/string-update runtime operations
   and shadowing-safe closure capture, then migrate compiler internals and stdlib methods while the old
   mutable syntax remains temporarily accepted. Keep internal raw mutation unobservable. Gate:
   alias-old-value tests across structs/collections/closures, structural-sharing black-box tests and
   formatter idempotence. Reject `let mut`, assignment, mutable fields/methods and `while` only at N,
   with its compile-fail migration corpus; runtime must land first so stdlib can migrate before the
   coherent public removal.
3. **C — checked effects and FFI.** Parse `effect`, `!()`, compositions, `!_`, and `!E`; infer body
   rows, check closed upper bounds and interface narrowing, serialize rows in module interfaces, and
   require externals/intrinsics to declare audited effects. Gate: pure/over-approximate/inferred-row
   tests, generic forwarding and ambiguity tests, cross-module invalidation, FFI undeclared-effect
   negatives. This precedes async, effectful iterators, resources and semantic I/O.
4. **D — equality/hash capability cut.** Separate partial equality from lawful reflexive equality and
   hashability; derive complete-structure operations only when every field qualifies; reject functions,
   floats as default keys, and opaque externals without impls. Then bind HAMT key acceptance to `Hash`
   - lawful equality. Gate: NaN/non-equalable compile failures, hidden-field/context-independent
     equality, unordered map equality, equal⇒same-hash property tests, embedding normalization (after G).
5. **E — persistent iterator protocol.** Replace `mut next(): Option<Item>` with successor-state
   `next(): Option<#(Item,self)> + !E`; rewrite adapters/terminals in Nymph, iterable forwarding and
   `for` lowering; remove general `while`. Gate: replayability/persistence, sequential latent-effect
   order, early break/continue, chained collection forwarding, and no mutation in public stdlib.
6. **F — structs/privacy/debug.** Enforce package/module field availability; add one-source whole spread,
   opaque clone/update rules and shape-only patterns; derive constructor availability from fields.
   Lower source-visible field sets into `echo`/debug metadata while equality/hash retain all fields.
   Gate: three-context visibility matrix for fresh/clone/update/access/pattern/default; duplicate spread;
   hidden equality and redacted debug. Package identity must exist before `internal` is meaningful.
7. **G — conversion/error graph.** Add enum embedding AST, DAG/unique-normalized-path analysis, nested
   nominal representation/matching, generated pure `Into`, explicit generic argument completion, and
   finally `?`'s unique pure infallible error conversion. Gate: cycle/diamond/ambiguity failures,
   selected-field preservation, deep wrappers/method dispatch, transparent equality/hash, exact vs
   converted `?`, and no Option↔Result implicit conversion. Embedding depends on D for normalized
   equality and C so `?` can prove conversion purity.
8. **H — cold structured tasks.** Add async AST/HIR and runtime recipe/default-handle/task-context state
   machine: cold construction, memoized direct await, fresh spawn, handle `Result`, join-on-context-exit,
   panic isolation, deterministic select, race ownership, cooperative cancellation/AbortSignal bridge.
   Gate each runtime primitive with deterministic scheduler tests, then compiler effect-placement tests
   (`Task` construction pure; drive/spawn performs `!E`; handle await pure), nested-Result and valid-main
   integration tests. Do not represent tasks as bare eager JS Promises.
9. **I — exact numerics/ranges/indexing.** Choose a 64-bit JS representation, implement checked
   arithmetic/casts and range-fact analysis, then indexing/range slicing (negative code-point indices,
   tuple restriction). Gate boundary/overflow/div-zero/shift tests, proven-check elision snapshots,
   conversion proof tests, reversed/OOB range matrices, Unicode strings. This replaces current Number
   payload assumptions and 2^53 stdlib limits before persistent collections freeze their ABI.
10. **J — proper tail calls.** After A fixes callable HIR, add a backend-independent tail-position pass
    and prototype the trampoline/loop calling convention for self/mutual/generic/higher-order/branch/
    match calls. Gate deep-stack execution tests for every promised category and JS snapshots. Finalize
    the calling convention only after H/K fixes suspension, context joining and cleanup continuations;
    unrelated numeric, iterator and enum work need not block the prototype.
11. **K — managed resources.** Parse/lower `let use`, validate synchronous pure/non-fallible idempotent
    `Close`, emit reverse lexical cleanup over normal/`?`/return/panic/cancellation, aggregate suppressed
    defects, and add capture-vs-join warnings. Gate a trace-resource matrix for all exits, multiple close
    failures, escaped-closed aliases, and four-span child-capture diagnostics. Requires H contexts and C
    effects; it constrains J tail position.
12. **L/M/N — tooling and public cutover.** Project all new nodes/facts through stable analysis; add
    completion/hover/symbol/rename/token handling and cancellation-safe diagnostics; teach formatter and
    TextMate grammar; migrate stdlib, examples, docs/reference and executable doc fences; replace old
    mutation goldens. Gate: focused crate tests, `cargo nextest run`, doctests if introduced,
    `cargo clippy --all-targets --all-features`, `pnpm lint`, docs build, extension compile/tests, plus a
    release-mode `echo` warning/omission test. Cut syntax only once compiler, stdlib and tooling migrate
    in one compatibility window.

## Hidden ordering constraints and contradictions

- “All ordinary values immutable” conflicts directly with current language/stdlib/HIR transactional
  mutation. Transaction rollback helpers are a REPL implementation concern, not a value-semantics seam
  to expose (`crates/nymph-codegen/src/hashmap_runtime.js:3-44`; HIR mutation evidence above).
- ADR-0002's “every value boxed” is implemented only for user-visible values; HIR intentionally keeps
  raw loop counters (`crates/nymph-hir/src/hir.rs:171-192`). Exact 64-bit arithmetic cannot retain raw JS
  Number fast paths without proofs/conversion, so I precedes final loop/runtime optimization.
- Runtime structural equality currently gives every box equality/hash, contradicting static conditional
  capabilities and float hash restrictions. D must change both solver and runtime together; merely
  changing stdlib interfaces leaves maps unsound.
- Field visibility exists in syntax/interface metadata but constructor checking ignores it; package
  visibility cannot be finished until package identity is defined. Debug currently reveals all keys,
  so F requires source-location metadata or compiler-selected rendering, not only a runtime method.
- Current `Option`/`Result` cross-API is split to avoid an import cycle
  (`stdlib/src/convert.nym:1-18`). Generated `Into` must be compiler-owned or emitted into an acyclic
  canonical owner; placing generated impls into both enum modules recreates this ordering problem.
- Persistent iterators require recursive `self` in the successor type and effect rows. Therefore A/C
  precede E; removing `while` before migrating stdlib makes the compiler unable to build itself.
- Async cleanup and PTC are mutually constraining: task-context join and `let use` cleanup are pending
  work after a tail call. The cleanup continuation ABI must be fixed before claiming J/K completion.
- `echo` is syntax plus build-mode policy plus visibility-aware debug, not an `Io` intrinsic. It belongs
  after F and C and requires CLI/build configuration before docs advertise release behavior.
- Root async result shapes are designed, but exact root error rendering/exit codes are explicitly open;
  compiler acceptance and CLI process policy should be separate boundaries.

## Map fog now precise enough for follow-up tickets

The map's existing representation tickets already cover exact 64-bit boxes, persistent collections
plus lawful equality/hash, effect-row inference, cold structured tasks, and cleanup-aware continuations.
This research sharpened four additional questions enough to ticket separately:

1. [Define package identity and internal visibility ownership](https://github.com/TheOnlyTails/nymph_lang/issues/90)
   — decide manifest/interface identity and canonical generated-implementation ownership.
2. [Define source-aware echo and release-build policy](https://github.com/TheOnlyTails/nymph_lang/issues/91)
   — prevent hidden-field leaks while preserving non-semantic observation.
3. [Define root result rendering and process exit policy](https://github.com/TheOnlyTails/nymph_lang/issues/92)
   — keep Node host behavior separate from task and expected-error semantics.
4. [Choose the range-analysis architecture and proof contract](https://github.com/TheOnlyTails/nymph_lang/issues/93)
   — prototype the lattice/query seam and distinguish diagnostic from optimization proofs.

The largest uncertainty is not where the work lives but the four unchosen runtime ABIs: exact 64-bit
numbers, persistent collections, cleanup-aware tail calls, and cold structured tasks. Those should be
resolved/prototyped before scheduling the public syntax cutover; the existing session, canonical
emission/boxing, external-linkage, diagnostic, formatter and LSP seams are sufficiently concrete to
plan around now.
