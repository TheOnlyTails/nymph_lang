# Integrated compiler performance pass

## Goal

Reduce cold and incremental latency for both single-file and multi-module compilation without introducing a second compiler pipeline or weakening the stable-runtime invariants established by the architecture-remediation stack.

The controlled pre-pass A/B measurement against `ab7566ec4722` found median task-clock regressions of 11.8% for a tiny check, 13.3% for a tiny compile, and 5.6% for `1.compare_to(2)` compile plus Node. The three-module compile-plus-Node result regressed 1.0%, which is within the observed 6–7% sample variation. Median process RSS was effectively unchanged.

## Invariants

- Preserve exact `DefinitionId` and `RuntimeAssemblyPlacement` end to end.
- Preserve per-module Salsa caching and equal-output backdating.
- Preserve deterministic source, definition, assembly, and link order.
- Missing exact facts remain typed errors or explicit recovery, never guessed fallbacks or silent absence.
- Do not introduce compatibility flattening, dependency-body access, emitted-JS scanning, identity reconstruction, or a separate single-file compiler.
- Preserve the `|>` operator, nominal `==`/`!=`, and ordinary explicit `.equals()` dispatch.

## Measurement design

Use release binaries built from isolated Jujutsu workspaces with the same pinned toolchain and profile. Warm builds before measurement and run samples serially and uncontended. Alternate baseline and candidate samples to limit drift.

Measure these scenarios:

1. tiny single-file check;
2. tiny single-file compile;
3. `1.compare_to(2)` compile plus Node;
4. representative three-module check and compile plus Node;
5. retained-session unchanged warm compile;
6. retained-session private-body edit;
7. retained-session public-interface edit.

Report repeated-sample medians and coefficient of variation for wall, user, system, task-clock, and maximum RSS. Record emitted bytes, relevant Salsa query execution counts, and equal-output backdating evidence. Attribute parsing, checking, lowering, assembly, bundling, and Node costs only where direct instrumentation separates them. Use the existing counting allocator only when the same benchmark closure and fixture are available at both revisions.

## Diagnosis sequence

Profile before changing behavior. Test these ranked hypotheses independently:

1. eager ambient-core analysis dominates tiny programs;
2. runtime-role or runtime-manifest construction is repeated or demanded earlier than needed;
3. stable identity hashing and graph-closure work is excessive on both paths;
4. callback source acquisition duplicates meaningful parser work before the Salsa graph;
5. bundling dominates compile after semantic work is reduced.

Each probe must distinguish one hypothesis using query events, phase counters, sampling profiles, or a narrowly tagged temporary counter. Temporary instrumentation must be removed before committing unless it is justified as durable benchmark observability.

## Optimization strategy

First optimize shared query demand. Runtime roles, manifests, lowering, assembly, linking, and bundling should execute only when the requested operation and exact source demands require them. A check must not perform emission-only work. A tiny compile that does not use a runtime protocol must not eagerly lower unrelated runtime definitions.

If profiling demonstrates material general-graph overhead after shared work is lazy, add a single-module specialization inside `CompilerSession`. The specialization may avoid provider traversal and general graph closure construction, but it must feed the same semantic, runtime-manifest, stable-lowering, assembler, and link-plan queries with the same keys and diagnostics. It must not become a second implementation of checking or lowering.

Do not add a specialization based only on aggregate timing. If shared lazy evaluation removes the material difference, retain one path.

## Correctness and performance gates

For each optimization:

1. add a RED query-count or performance regression test at the real call seam;
2. preserve exact diagnostics and byte-identical HIR/JS for representative cases;
3. run focused single-file and three-module invalidation tests;
4. run Node-backed `compare_to`, nominal equality/explicit `.equals()`, pipeline, and representative project cases;
5. rerun the controlled A/B harness;
6. commit only if the target gain exceeds sample variance and no measured workload regresses materially.

Use independently valid commits for shared laziness, any justified single-module specialization, and benchmark-observability changes. A failed or inconclusive optimization is reverted rather than retained as speculative complexity.

## Scope boundaries

The pass may fix bounded integration defects discovered by profiling or verification. It will not redesign stable bodies, implementation catalogs, runtime-role registration, or namespace summaries. Those require separate ownership changes if their residual risks become behaviorally reproducible.

The strict link-plan integration fix already in progress remains a separate correctness commit and will be verified before performance implementation begins.
