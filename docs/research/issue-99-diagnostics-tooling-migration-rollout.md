# Issue #99: diagnostics, tooling, documentation, and migration rollout

## Status and conclusion

This artifact resolves [issue #99](https://github.com/TheOnlyTails/nymph_lang/issues/99) as a
planning decision. It is based directly on `language-identity-plan@origin` commit
`56f05139c160`; it does not implement the destination language or create execution tickets.

The rollout must remain the compiler rollout decided by
[#95](https://github.com/TheOnlyTails/nymph_lang/issues/95): R0 through R12 build complete
semantic and runtime foundations behind stable compiler contracts, R13 proves that every user-facing
consumer is ready, and R14 performs one coherent public immutable cutover. The repository must never
publish a partly migrated language in which the parser accepts a construct that the formatter, retained
session, HIR, backend, standard library, or normal diagnostic path cannot handle.

No settled decision is contradictory, infeasible, or prohibitively expensive. No human-in-the-loop
decision is required. Four stale draft passages were implementation hazards rather than open product
questions; this change corrects them:

1. `echo` is visibility-independent and recursively renders complete ordinary structure. It never
   dispatches `Debug`; functions, resources, and opaque externals use inert placeholders.
2. Entrypoints use the accepted root shapes and exact 0/1/130/143/101 Node launcher policy from #92.
3. Enum embedding is fixed-point set inclusion over static views. Cycles and diamonds deduplicate,
   wrapper construction and source-spread patterns are removed, and embedding generates no `Into`.
4. Iteration returns nominal `Iteration<Item, Next>`, `for` remains dedicated HIR, and immutable state
   `loop` with simultaneous named `continue` replaces source `while`.

These corrections are in
[`language-identity.md`](../design/language-identity.md) and
[`async-model.md`](../design/async-model.md). `CONTEXT.md` already states the settled immutable and
activation-owned direction and needs no correction for #99.

## Settled inputs this plan does not reopen

The rollout consumes #84 through #98 as one contract:

- managed references and `Close<!E>` from #84;
- canonical finite effect sets and the subset fixed-point solver from #85;
- exact BigInt-backed 64-bit integers and checked conversion policy from #86/#93;
- persistent collections and complete lawful equality/hash from #87;
- execution frames and one activation machine for calls, suspension, cancellation, cleanup, and proper
  tail calls from #88/#89;
- exact `PackageId`, contextual visibility, and complete hidden structure from #90;
- complete `echo`, compiler-owned profiles/lints, and release erasure from #91;
- the root launcher matrix from #92;
- opaque fingerprinted FFI identities and backend-neutral adapters from #94;
- the stable identity/session/HIR rollout and R0-R14 order from #95;
- named-only structs with one exact leading clone/update spread from #96;
- fixed-point enum sets and explicit-only `Into` fallback from #97; and
- persistent `Iteration`, latent effects, dedicated `For`, functional state loops, and no `while` from
  #98.

## Current repository evidence

### Canonical diagnostics and snapshots

- `crates/nymph-diagnostics/src/lib.rs:12-205` owns one shared `Diagnostic` and terminal renderer. A
  diagnostic currently has severity, string code, message, primary span, secondary labels, notes, and
  one help string. It has no structured edit payload.
- Lexer/parser diagnostics are typed catalogs in `crates/nymph-syntax/src/errors.rs:16-207`, with
  numeric prefix 0/1. Sema's typed `TypeError` catalog is in
  `crates/nymph-sema/src/errors.rs:15-220`, with prefix 2. The derive macro assigns four-digit numbers
  from declaration order (`crates/nymph-errorcode/src/lib.rs:38-63`), so existing variants must not be
  reordered; migration variants append until codes have an explicit stable allocation scheme.
- The compiler attributes and folds parser, graph, import, interface, and body diagnostics in
  `crates/nymph-compiler/src/project/queries.rs:2485-2525` and preserves recovered-parser checking in
  the authoritative project fold at lines 2721-2770.
- Stable lowering already has typed internal errors, while project emission currently maps failures to
  synthetic span-zero `STABLE-*` diagnostics
  (`crates/nymph-compiler/src/project/emission.rs:20-31,104-138`). This is an invariant path, not an
  acceptable first rejection of valid source.
- Existing diagnostic rendering tests cover UTF-8 spans in
  `crates/nymph-diagnostics/tests/utf8_span.rs`; parser, sema, compiler, and LSP tests mostly assert
  fields or messages directly. There is no repository `.snap`/`insta` diagnostic corpus and no
  snapshot of structured edits, rendered terminal output, and LSP output from one cause.

### Retained compiler sessions and LSP consumers

- `CompilerState` retains a normal `CompilerSession`, a separate no-implicit-prelude standard-library
  session, overlays, effective source ownership, and diagnostic publication ownership
  (`crates/nymph-lsp/src/compiler_state.rs:166-230`). It must remain one long-lived graph per mode;
  migration must not introduce a side compiler or reparsing cache.
- LSP diagnostics use immutable worker snapshots, cancellation, document revisions, and compiler-owned
  project ordering
  (`crates/nymph-lsp/src/diagnostics.rs:27-85,143-210`). The conversion carries ordinary diagnostics
  only. The server advertises no code-action provider (`crates/nymph-lsp/src/lib.rs:173-206`).
- Completion, hover, definitions, references, rename, document symbols, formatting, and semantic tokens
  consume the same retained analyses. Semantic tokens contain exhaustive source-AST walks and still
  cover `Type::Mut`, assignment, and `While`; they must change in the same vertical slices as the AST.
- `crates/nymph-lsp/tests/incremental_session.rs` already exercises imports, overlays, reused sessions,
  completions, hovers, and semantic tokens. It is the right home for no-second-session, no-stale-fix,
  normal/no-prelude, and profile-switch parity tests.
- Neither the LSP nor extension has workspace profile/lint configuration. The extension manifest exposes
  only `nymph.server.path`.

### Formatter and other AST consumers

- `nymph-format::format` reparses through the canonical parser and returns no output for malformed or
  recovered input (`crates/nymph-format/src/lib.rs:49-70`). Therefore formatting is never a migration
  engine for rejected legacy syntax.
- Its AST walk still handles assignment and `While`
  (`crates/nymph-format/src/lib.rs:410-590`) and helper logic knows those nodes at lines 594-638. Every
  R-stage syntax/AST change must update this walk, LSP semantic-token walks, query walkers, and test
  helpers before that syntax is considered complete.
- Formatter fixtures assert exact output, parseability, token-level semantic preservation, and second-pass
  idempotence
  (`crates/nymph-format/tests/fixtures.rs:103-166`). The corpus test sweeps every `.nym` file under
  `stdlib/src` and `examples` (`crates/nymph-format/tests/corpus.rs:6-40`).

### Extension highlighting

- The TextMate grammar's control keywords still include `while`; modifiers include `mut`; compound
  assignment receives a dedicated scope
  (`extension/syntaxes/nymph.tmLanguage.json:103-131,170-220`). Destination `effect`, `echo`, and
  `loop` are absent, and contextual `let use` has no grammar rule.
- The grammar remains the lexical fallback; server semantic tokens are authoritative after attachment.
  Grammar changes nevertheless need token/scope fixtures so detached editing does not teach retired
  syntax.
- Current extension tests validate the sole `.nym` suffix and comment-delimiter agreement, not keyword
  inventory or TextMate scopes. The extension scripts already provide compile, unit, configuration,
  smoke, docs, bundle, and package checks in `extension/package.json`.

### Manifests, CLI, profiles, and lints

- `Manifest` currently owns package, dependencies, and build settings only; there is no general
  `[lints]` table (`crates/nymph-project/src/lib.rs:19-78`). Existing tests cover defaults, schema
  errors, discovery, and canonical TOML round trips at lines 366-429.
- The CLI has only the global `--manifest` selector
  (`crates/nymph-cli/src/main.rs:21-43`). `build`, `check`, and `run` do not accept `--release`; shared
  project dispatch calls one-shot compiler entrypoints without profile/lint inputs
  (`crates/nymph-cli/src/project_support.rs:80-150`).
- `run` appends a direct `main()` call to an otherwise inert module and returns raw Node status
  (`crates/nymph-cli/src/commands/run.rs:47-60,93-123`). R12 replaces that executable path with the
  exact launcher policy; ordinary build output remains inert.
- `BuildProfile` and `LintLevel` are not compiler/session inputs yet. Per #91, the compiler—not the
  project parser, CLI, LSP, or extension—must own their meaning and the `echo-in-release` diagnostic.

### Documentation, examples, standard library, and migration material

- VitePress navigation still presents a `Mutability` reference page
  ([`config.ts`](../.vitepress/config.ts#L55-L74)). Current reference, guide, and standard-library pages
  contain mutable bindings, mutating methods, assignments, and `while` examples.
- The doc-sample harness checks only exact `nym` fences through retained compiler sessions; `nymph`
  fences are intentionally illustrative and skipped. It parses and type-checks but never lowers,
  emits, or runs Node
  (`crates/nymph-compiler/tests/docs_samples.rs:1-26,201-273`).
- Mutable example hotspots are `word-frequency`'s map accumulator, `shapes`' sum, and `todo-cli`'s
  mutable store, receiver, fields, and list. Every example's root shape also needs R12 validation even
  when its body is already immutable.
- Standard-library migration is concentrated in `stdlib/src/iter/mod.nym`,
  `stdlib/src/iter/iterable.nym`, range and collection modules, `string.nym`, and `ops/mod.nym`. Host
  adapters in `collections/list.ts`, `collections/map.ts`, `display.ts`, `hash.ts`, and
  `ops/equality.ts` expose the old mutating iterator/collection or blanket debug/equality paths.
  Ordinary stdlib behavior should move to Nymph wherever practical; TypeScript remains for host/runtime
  primitives.
- There is no dedicated frozen legacy migration corpus or migration command/script. Current related
  coverage is spread among parser tests, formatter fixtures/corpus, compiler doc samples, CLI tests, and
  compiler Node-execution tests. Legacy fixtures must live outside accepted `.nym` source corpora so
  their intentional errors cannot look like current examples or standard library.

## Component ownership

| Component                            | Accountable owner                                                                     | Required outcome                                                                                                                                                                                                                                                                                                                                                     |
| ------------------------------------ | ------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Canonical diagnostic and edit schema | `nymph-diagnostics`                                                                   | Add one backend-neutral atomic edit payload with title, exact source/module identity, byte spans, replacement text, and `MachineApplicable` applicability. Manual migrations carry explanation/help and no edit. Preserve stable codes, labels, notes, terminal rendering, and deterministic ordering.                                                               |
| Removed grammar and parser recovery  | `nymph-syntax`                                                                        | Recognize retired spellings only in a bounded recovery path, emit one causal parser diagnostic at the retired token/form, recover enough structure for independent diagnostics and proven edits, and never build an accepted destination AST node from it.                                                                                                           |
| Sema and interface-only diagnostics  | `nymph-sema` semantic/interface queries                                               | Own meaning-dependent diagnoses: resolved binding writes, mutating dispatch, visibility, struct field identity/order, enum set assignability, effects, range proofs, iterator capabilities, root shape, and lint policy. Header/interface errors must appear from complete/recovered interface analysis without requiring a body; body errors remain per-definition. |
| HIR and stable lowering diagnostics  | `nymph-sema` stable lowering plus `nymph-compiler` project fold                       | Treat `StableLoweringError` and missing stable artifacts as compiler invariant failures anchored to the responsible `DefinitionId`/source span. Any foreseeable source rejection moves earlier to parser/sema. Codegen must never be the first component to reject user source.                                                                                      |
| Formatter                            | `nymph-format`                                                                        | Format every accepted destination AST form, remove retired AST branches, preserve comments and precedence, and remain idempotent. Reject recovered legacy input; do not silently apply migration edits.                                                                                                                                                              |
| Retained session and LSP             | `nymph-compiler::CompilerSession` and `nymph-lsp`                                     | Carry profile/lint inputs and edit-bearing diagnostics through the existing long-lived normal/no-prelude graphs. Publish canonical diagnostics and expose only `MachineApplicable` edits as version-checked code actions. Keep all feature snapshots on the same source revision and recovered/complete interface.                                                   |
| Extension highlighting/configuration | `extension`                                                                           | Update lexical grammar and scope tests for destination and retired tokens; keep semantic tokens authoritative. Select the LSP release-diagnostic profile through workspace configuration without reinterpreting lints.                                                                                                                                               |
| CLI and manifests                    | `nymph-project` for schema/filesystem; compiler for policy; `nymph-cli` for selection | Parse the general `[lints]` table, pass it unchanged as compiler lint inputs, add `--release` to build/check/run, keep REPL development-only, and use the R12 launcher. Add a temporary explicit migration driver that applies only compiler-proven edits and otherwise reports manual sites.                                                                        |
| Reference and guide                  | `docs/reference`, `docs/guide`, VitePress config, doc-sample harness                  | Replace implemented-language mutable teaching with immutable values, effects, resources, state loops, persistent iteration, structs/enums, profiles, roots, and a migration guide. Make executable destination samples `nym`; reserve `nymph` for non-executable grammar sketches. Add selected emitted/Node doc tests where runtime behavior matters.               |
| Examples                             | `examples`                                                                            | Migrate all checked-in examples at R14, preserving demonstrated behavior and updating manifests/readmes. Compile every example; execute deterministic non-service examples and smoke service examples with bounded harnesses.                                                                                                                                        |
| Standard library                     | `stdlib` with compiler runtime adapters                                               | Land `Iteration`, adapters, terminals, persistent collection operations, and functional accumulation atomically with R10/R14. Keep only private runtime mutation and remove source-visible mutation and old adapter ABIs.                                                                                                                                            |
| Compatibility and removal            | cross-component cutover owner created by #100                                         | No semantic compatibility mode and no dual accepted syntax. Keep old implementation paths private only while foundations land. At R14 migrate all producers, switch old syntax to recovery-only errors, delete old AST/sema/HIR/runtime paths in dependency order, and retire temporary migration recognition/driver last after the documented migration window.     |

The edit payload is deliberately part of the canonical diagnostic rather than an LSP-only type. The
CLI migration driver and LSP code actions must consume exactly the same edits. An edit group is atomic,
non-overlapping, and tied to the source versions analyzed; consumers reject stale groups rather than
rebasing them heuristically. There is no `MaybeIncorrect` or hidden best-effort mode: if the compiler
cannot prove semantic preservation, it emits no machine edit.

## Diagnostic and migration contract

### Causal diagnostics

Each retired construct gets one root diagnostic, with secondary labels for the binding, field,
declaration, source enum, or proof that explains it. Diagnostics caused solely by the recovery node are
suppressed; unrelated errors in the recovered module remain. Snapshot ordering is parser/graph/import,
then interface/header, then source-ordered body diagnostics, matching the existing project fold.

The migration catalog must cover at least:

- every `mut` position (`let mut`, parameter/type/field markers, `mut func`, mutable static/member forms);
- simple and compound assignment, with the resolved target and mutating operation identified;
- `while` and old mutating iterator `next(): Option<Item>`/adapter contracts;
- positional struct construction and patterns, spread count/position, exact-source mismatch, unknown,
  duplicate, inaccessible, missing, and defaulted fields;
- enum wrapper construction, removed source-spread patterns, invalid set views, equality with no overlap,
  and missing/ambiguous/fallible/effectful explicit `Into` fallback;
- unproved integer widening, distinguishing proved-safe explicit insertion from a policy choice;
- echo/profile lint settings, invalid `[lints]` values, and release diagnostics;
- root-shape and `E: Display` errors before launcher emission; and
- destination iterator capability/effect, state-loop replacement, cleanup, and control-target errors.

Parser-owned diagnostics explain spelling and shape; sema-owned diagnostics explain meaning. A valid
program can never first fail in HIR or codegen. The diagnostic snapshots must record, from one fixture:

1. structured code/severity/message, module, primary and secondary spans, notes/help, and atomic edits;
2. stable terminal rendering, including UTF-8 and multi-line spans;
3. LSP code, UTF-16 ranges, related information, and matching code action; and
4. the no-edit manual diagnosis for the same construct under an unsafe semantic context.

### Machine-applicable edits

An edit is offered only under all listed proof conditions:

| Legacy form                          | Safe edit                                                                              | Required proof                                                                                                                                                                                                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Unused mutability marker             | Remove `mut`.                                                                          | The resolved binding/parameter/receiver/field has no writes, no mutating-only dispatch, no mutable ABI requirement, and removing the marker changes no overload.                                                                                                  |
| Simple local reassignment            | Replace `x = expression` with `let x = expression`.                                    | `x` resolves to one local in the same lexical block; the write dominates all later uses it is meant to update; initializer evaluation sees the same old binding; there is no branch/loop/closure capture, managed cleanup, defer/order, pattern, or scope change. |
| Compound local assignment            | Replace with `let x = x op rhs`.                                                       | The simple-reassignment proof holds, operator dispatch is exactly the checked compound operation, both operands are evaluated once in the same order, and no getter/index/field/alias participates.                                                               |
| Positional fresh struct construction | Insert exact field names without moving expressions.                                   | Sema knows the exact nominal type, complete declaration order and identities, every named field is available, arity is exact, and defaults/evaluation order are unchanged.                                                                                        |
| Positional struct pattern            | Insert exact field names and, when required, trailing anonymous `...`.                 | The nominal type and each field identity are exact, bindings keep the same source subvalue, omissions are known, and visibility makes the resulting pattern legal.                                                                                                |
| Enum wrapper expression              | Replace with an explicit static view (`as Destination`) or remove a redundant wrapper. | Fixed-point sets prove direct assignability and the old wrapper has no observable evaluation, identity, method-dispatch, equality, or conversion difference. The source expression remains single-evaluated.                                                      |
| Single selected enum pattern         | Replace with its qualified source variant.                                             | The recovered pattern denotes exactly one stable source variant; no arm expansion, guard duplication, binding-view change, reachability change, or exhaustiveness change occurs.                                                                                  |
| Implicit integer widening            | Insert the already selected explicit checked cast/conversion.                          | `RuntimeAnnotations` contains an auditable range proof that the conversion cannot trap and the selected operation returns the same exact value.                                                                                                                   |

The temporary migration driver has `--check` (report edits/manual blockers without writing) and
`--write` (apply complete, current-version atomic groups). It never formats a rejected file. After edits
make a file valid destination source, normal formatting is a separate pass. A repository run exits
nonzero when any manual site, stale edit, parse failure unrelated to recognized legacy syntax, or
post-edit compiler error remains.

### Manual migrations

The following cases intentionally receive causal diagnostics and migration-guide examples but no edit:

- field, index, collection, aliased, getter, or old-value-observing mutation;
- assignments inside branches, loops, matches, closures, or captured-variable flows;
- any rewrite that could change expression evaluation/count/order, shadowing scope, or cleanup timing;
- `while`, because choosing loop state, condition timing, simultaneous replacements, and exit cleanup is
  semantic design work;
- mutating iterators/adapters/terminals, because successor state, replay, latent effect count/order,
  short-circuiting, and exact-size capabilities must be chosen;
- a `mut func` or mutable receiver that actually mutates or participates in mutating-only dispatch;
- resource fields/parameters and state-loop managed replacements, where acquisition and close order
  matter;
- struct cloning/default migrations affected by private/internal visibility, hidden fields, owner-run
  defaults, or opaque exact-source requirements;
- enum changes that require arm expansion, static-view/method-dispatch choices, equality behavior, or a
  new explicit conversion;
- integer conversion where checked/trapping/optional/result/wrapping behavior is a choice; and
- any case affecting evaluation, cleanup, visibility, enum views, iterator effects, or loop state.

## Dependency-ordered rollout

All R0-R12 work may land internally in dependency order, but destination syntax remains unpublished
until R14. At every stage, a syntax form is complete only when parser/recovery, formatter and all AST
walkers, sema, complete/recovered interfaces, `RuntimeAnnotations`, stable lowering, HIR, backend,
normal diagnostics, and focused tests agree.

| Stage                                   | Compiler/runtime dependency                                                                                                                                                                                                                      | Tooling, diagnostics, docs, and migration gate                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **R0: stable contracts**                | Freeze `PackageId -> ModuleIdentity -> DefinitionId -> BodyNodeId`; retain source-local parser `NodeId`; keep one `CompilerSession`, complete/recovered interfaces, `RuntimeAnnotations`, per-definition artifacts, and `StableLoweringContext`. | Extend canonical diagnostics with atomic edits and source-version identity; add structured/terminal/LSP snapshot harnesses and the frozen legacy corpus format. Add compiler-owned profile/lint inputs to the retained graph before frontends select them. Inventory every AST/query/formatter/LSP/extension/runtime consumer and every old-path search.                                                                                                                                                                                                     |
| **R1: package identity**                | Thread exact resolved package/module ownership through interfaces and sessions.                                                                                                                                                                  | Make manifest/project/LSP discovery produce the same `PackageId`; snapshot all public/internal/private diagnostics under root, dependency, std, loose, untitled, and no-prelude modes. This is required before visibility-sensitive struct diagnostics and root-only echo linting.                                                                                                                                                                                                                                                                           |
| **R2: effects and ABI**                 | Land canonical effect sets, subset solver, exported rows, and ABI fingerprints.                                                                                                                                                                  | Add effect syntax/recovery/formatting/highlighting and interface-only diagnostics; update hover/completion/semantic tokens; document canonical rows. Snapshot latent rows through calls, tasks, iterators, cleanup, and FFI.                                                                                                                                                                                                                                                                                                                                 |
| **R3: exact integers and range proofs** | Use exact 64-bit representation and auditable proof decisions in `RuntimeAnnotations`.                                                                                                                                                           | Diagnose removed unconditional widening; attach an edit only when the proof establishes the identical exact value. Document checked/default and explicit conversion families; add formatter, hover, HIR, and Node boundary snapshots.                                                                                                                                                                                                                                                                                                                        |
| **R4: persistent values**               | Land persistent list/map/set representations and lawful complete equality/hash.                                                                                                                                                                  | Diagnose source-visible collection mutation and mutable wrappers; automate only unused marker removal or strictly proven local shadowing. Add immutable/persistent reference material before migrating examples. Keep host mutation private and verify it cannot surface through interfaces or LSP types.                                                                                                                                                                                                                                                    |
| **R5: structs**                         | Land complete field identities, contextual availability, owner-run defaults, `StructFresh`, and `StructCloneUpdate`.                                                                                                                             | Land named/spread parser, recovery, formatter, semantic tokens, interface-only visibility diagnostics, and safe positional-label edits. Add the three-context docs/migration matrix and snapshots for hidden-field clone preservation and pattern `...`.                                                                                                                                                                                                                                                                                                     |
| **R6: enums**                           | Land fixed-point variant sets, relevant generic projections, static-view dispatch, equality/hash, and backend-erased view HIR.                                                                                                                   | Correct all wrapper/DAG/generated-`Into` diagnostics and docs. Offer edits only for exact direct set views or one qualified variant. Add cycle/diamond/generic identity, matching, dispatch, deep `?`, formatter, LSP, and extension fixtures.                                                                                                                                                                                                                                                                                                               |
| **R7: activation ABI**                  | Establish one backend-neutral activation representation for calls, tail transfer, suspension, cancellation, and cleanup.                                                                                                                         | Give every control/cleanup diagnosis a source and stable definition anchor. Add HIR snapshots and ensure activation failures are invariant diagnostics rather than backend panics. No new user-facing partial control syntax is published.                                                                                                                                                                                                                                                                                                                   |
| **R8: async**                           | Land task/execution frames, effect rows, cancellation, structured joins, and cleanup on the activation machine.                                                                                                                                  | Update async syntax consumers, semantic tokens, reference material, examples, and retained-session hover/diagnostics together. Snapshot every exit and cleanup route; keep expected `Result`/`Option` distinct from cancellation/defects.                                                                                                                                                                                                                                                                                                                    |
| **R9: resources and FFI**               | Land `Close<!E>`, managed cleanup, opaque external identities, fingerprinted adapters, and Node registry.                                                                                                                                        | Add `let use` contextual highlighting, formatter and diagnostics; document alias/repeated close and trusted ABI boundaries. Keep resource/visibility/default migrations manual. Verify echo placeholders invoke no host/user callbacks.                                                                                                                                                                                                                                                                                                                      |
| **R10: iteration and control**          | Land nominal `Iteration`, capability-preserving `self`, latent effects, dedicated `For`, state `loop`, named simultaneous `continue`, and activation-owned cleanup.                                                                              | Atomically migrate iterator interfaces, adapters, terminals, ranges, collections, and compiler runtime roles. Add `loop`/remove `while` across parser recovery, formatter, all walkers, LSP, extension, docs, and fixtures. Diagnose `while`/mutating iteration manually; verify effect order/count, replay, short-circuiting, exact-size behavior, every exit, cleanup, and constant stack.                                                                                                                                                                 |
| **R11: echo, profiles, lints**          | Land dedicated echo checked/HIR node, safe complete observer, development emission and release erasure.                                                                                                                                          | Add `[lints]`, `--release` for build/check/run, development-only REPL, LSP workspace release diagnostics, `echo` grammar/formatting/semantic tokens, root-package-only lint ownership, terminal OSC-8/redirected snapshots, and proof that release contains no observer/site URI. Frontends pass inputs; only the compiler interprets policy.                                                                                                                                                                                                                |
| **R12: roots**                          | Validate accepted semantic root shapes and add the separate Node launcher over produced/cancelled/defected outcomes.                                                                                                                             | Replace direct `main()` execution, document exact output/status matrix, add CLI and Node signal/cleanup tests, and update all example roots. Build output stays inert. Root shape errors remain normal sema diagnostics and LSP/CLI-identical.                                                                                                                                                                                                                                                                                                               |
| **R13: tooling and migration gate**     | Freeze the complete destination compiler/runtime/stdlib behavior from R0-R12.                                                                                                                                                                    | Require every diagnostic snapshot, proven edit/manual fixture, formatter idempotence test, retained-session/profile/code-action parity test, TextMate scope test, manifest round trip, reference/guide migration, doc sample, example, stdlib source, and removal inventory to pass on the candidate tree. Run the migration driver in `--check` mode and require no unowned source producer. No destination syntax is public before this gate is green.                                                                                                     |
| **R14: one public immutable cutover**   | Switch to destination semantics once; no compatibility execution mode.                                                                                                                                                                           | Migrate stdlib first, then examples, docs/reference, tests/fixtures, extension snippets, and every generated source producer in the same reviewable cutover. Change old grammar acceptance to causal recovery-only errors; delete mutable AST, sema, body facts, stable lowering, HIR/emitter, runtime and adapter paths in the #95 order. Run focused plus repository-wide gates and path searches. Keep only bounded recovery recognition/migration tooling for the documented transition, then remove those last without ever accepting old syntax again. |

### R14 deletion order

The deletion sequence is structural, not optional cleanup:

1. migrate checked-in and generated source producers;
2. turn parser acceptance of old forms into recovery-only migration diagnostics;
3. delete mutable/old-control source AST and token consumers except the isolated recovery recognizer;
4. delete semantic mutability, old conversions, mutating dispatch, and old interface shapes;
5. delete old stable body facts and `RuntimeAnnotations` entries;
6. delete old stable lowering and `lower_for`-to-mutable-`while` expansion;
7. delete old HIR and emitter consumers;
8. delete transactional source-assignment cells, mutating collection/iterator adapters, unchecked bridges,
   old external ABI projection, Number/BigInt bridges, and stdlib re-export shims; and
9. remove the migration-only recognizer, frozen compatibility allowlist, and temporary driver last, once
   one cutover release has shipped and the migration guide/corpus covers every remaining manual class.

Complete/recovered interfaces, stable lookup traits, one-shot facades over retained sessions, LSP's
normal/no-prelude sessions, synthetic packages, activation-private mutation, and persistent-collection
private builders are permanent boundaries, not compatibility leftovers.

## Verification gates

### Focused component gates

| Area                        | Gate                                                                                                                                                                                                                                                                                                                                                                    |
| --------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Diagnostics                 | Unit tests for stable append-only codes and edit validation; structured + terminal snapshots for every migration class; Unicode/multi-line/secondary-span coverage; one root cause without recovery cascades.                                                                                                                                                           |
| Parser/AST                  | Positive destination and negative recovery fixtures; retired forms produce exact code/span/edit/manual verdict; exhaustive visitor tests; no accepted old AST node.                                                                                                                                                                                                     |
| Sema/interfaces             | Complete and recovered interface snapshots, all visibility contexts, effect and enum fixed points, range-proof decisions, iterator capabilities, root validation, and proof that interface-only errors do not require body analysis.                                                                                                                                    |
| Stable lowering/HIR/codegen | Per-definition HIR snapshots for each destination operation, invalidation tests, source-anchored invariant failures, and negative tests proving no source reaches codegen as its first rejection.                                                                                                                                                                       |
| Formatter                   | Exact fixture output, parseability, semantic fingerprint, second-pass equality, range formatting, malformed/recovered no-output behavior, and full stdlib/examples corpus idempotence.                                                                                                                                                                                  |
| LSP                         | CLI/LSP diagnostic parity from the same session; UTF-16 edit ranges; code-action capability and current-version atomic workspace edits; stale revision cancellation; normal/no-prelude, loose/project/untitled, overlay/importer, and development/release profile parity; hover/completion/rename/definition/references/semantic-token behavior after every AST change. |
| Extension                   | JSON parse, destination/retired keyword and scope fixtures, `.nym`/Markdown injection coverage, TypeScript compile, unit/configuration/smoke tests, bundle and VSIX verification. Confirm TextMate fallback and semantic-token attached results agree on destination tokens.                                                                                            |
| Manifest/CLI                | `[lints]` schema/default/error/round-trip tests; `--release` for build/check/run and not REPL; root-package lint ownership; migration `--check`/`--write` atomicity and manual exit status; exact root launcher stdout/stderr/status/signal matrix.                                                                                                                     |
| Docs/reference              | Convert executable snippets to `nym`; compile-check all such fences; add selected emit-and-Node samples for echo erasure, integer boundaries, persistent iteration, cleanup, enum views, and roots; VitePress build plus internal-link and orphan-page checks.                                                                                                          |
| Examples                    | Check every manifest, execute deterministic CLI examples with expected output/status, and bounded-smoke service examples. Verify readmes use the exact source and commands.                                                                                                                                                                                             |
| Stdlib/runtime              | Compile and link all embedded modules; Node tests for persistent collections, equality/hash, exact integers, iteration/effects, resources/cleanup, tasks/activation, echo, FFI, and roots. Verify Nymph owns ordinary stdlib behavior and JS adapters expose only host/runtime primitives.                                                                              |
| Migration corpus            | Frozen `.nym.txt` legacy inputs plus machine-readable expected code/spans/applicability/edit/result. Apply every safe group, then parse, format twice, check, lower, and execute when behavior is observable. Manual fixtures must remain unchanged and identify the migration-guide anchor.                                                                            |

### Repository-wide cutover gate

The R14 candidate is not releasable until all of these pass together on Node 24 or newer:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features
cargo nextest run
cargo test --doc
pnpm lint
pnpm --filter nymph-docs build
pnpm --filter nymph compile
pnpm --filter nymph test:unit
pnpm --filter nymph test:configuration
pnpm --filter nymph test:smoke
```

`cargo nextest run` must include the compiler's Node-execution suites (`run_node`, `golden_programs`,
`std_linkage`, collections, boxing, arithmetic, projects, prelude, std I/O, and stable assembly) as well
as `nymph-codegen`'s structural HIR/emitter tests. CI must fail rather than skip when Node is absent.
The gate also runs every example and the migration/cutover inventory script; these should become named
workspace commands when #100 decomposes execution.

### Proof that removed paths are gone

The cutover inventory script must be parse-aware for source and maintain a small reviewed allowlist only
for frozen migration inputs and, while retained, the recovery recognizer. It must fail on:

- source spellings `let mut`, `mut func`, `mut` type/parameter/field markers, `while`, compound
  assignment, old enum wrapper/spread patterns, and `next(): Option<...>` in stdlib, examples, docs
  executable fences, extension snippets, parser success fixtures, and generated source;
- accepted syntax/AST symbols such as `Type::Mut`, `LetKind::Mut`, `FuncKind::Mut`,
  `ExprKind::AssignOp`, `ExprKind::While`, and `AssignOperator` outside recovery-only code;
- semantic symbols and cases such as `TyKind::Mut`, mutable members/receivers, old implicit widening,
  `StableExprKind::AssignOp`, `StableExprKind::While`, and old Option iterator runtime roles;
- HIR/emitter paths such as `HirExpr::Assign`, `HirExpr::While`, mutable local declarations, and the old
  `lower_for` expansion;
- runtime scaffolding such as `nymphCell`, `nymphCellGet`, `nymphCellSet`, source property assignment,
  mutating `NymphListIterator`/`NymphMapIterator`, old Option iterator adapters, old enum wrappers,
  Number/BigInt compatibility bridges, and obsolete stdlib external symbols; and
- release bundles containing echo observers, echo site/source URI metadata, mutable compatibility
  helpers, or launcher code in ordinary inert build output.

The script must distinguish legal private implementation mutation (persistent builders, activation
slots, host resource state) from deleted source/runtime compatibility paths by exact symbols and file
ownership; a broad ban on JavaScript assignment would be meaningless.

## Precise handoff to #100

After #99 closes, [#100](https://github.com/TheOnlyTails/nymph_lang/issues/100) becomes the sole map
frontier. It owns execution-ticket decomposition and verification gates, not new language design.

#100 must:

1. preserve R0-R14 and native blocker edges rather than creating layer-by-layer tickets that expose a
   partial public language;
2. create independently valid internal implementation tickets for the stable contract, R1-R12 vertical
   slices, the R13 tooling/migration gate, the R14 source cutover, ordered old-path deletion, and final
   compatibility-tool removal;
3. assign every row in the ownership table to one ticket and every focused/repository gate to an
   explicit blocking verification ticket or acceptance criterion;
4. keep parser recovery diagnostics and sema meaning diagnostics canonical, with one edit payload used
   by the temporary CLI migration driver and LSP code actions;
5. make stdlib iteration migration, compiler runtime-role replacement, examples/docs migration, and
   public syntax switch explicit blockers of the R14 cutover;
6. preserve the four corrected decisions above verbatim rather than inheriting the stale draft wording;
7. include the frozen legacy corpus and exact removed-path inventory before deleting compatibility code;
   and
8. create no compatibility execution mode, generated enum `Into`, visibility-filtered echo, open root
   policy, Option iterator ABI, or source `while` ticket.

There is no contradiction or HITL blocker to carry forward. The only frontier change from resolving
#99 is that #100's native blocker closes and decomposition may begin.
