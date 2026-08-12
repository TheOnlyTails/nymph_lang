# Parallel Incremental Compiler Cutover

## Status

Approved on 2026-07-26. This design supersedes the remaining differential and
atomic-cutover constraints in the incremental semantic compiler plan. The
project is pre-public, so compiler-internal compatibility is not a requirement.
The Nymph language and its observable runtime semantics remain stable.

## Goal

Finish the Salsa-backed interface-consuming compiler while removing the legacy
flattened compiler in parallel. Merge both efforts into one implementation that
has no compatibility pipeline, then verify language behavior and measure where
parallel execution can produce additional speedups.

## Base and workspace topology

Both implementation changes start from the independently green #78 foundation
commit `540ee045`.

```text
                         #78 foundation (540ee045)
                                    |
                  +-----------------+-----------------+
                  |                                   |
                  v                                   v
       Workspace A: new compiler          Workspace B: legacy removal
       Finish semantic correctness        Delete flattened compatibility
       and stable lowering inputs         internals and internal tests
                  |                                   |
                  +-----------------+-----------------+
                                    |
                                    v
                         Merge and verify one
                         incremental compiler
```

Each workspace uses narrow, independently valid, issue-aligned commits. Shared
files are expected, especially compiler session and query modules; merge
resolution must prefer the new interface pipeline and must not restore deleted
compatibility machinery.

## Workspace A: complete the new compiler

Workspace A owns replacement behavior:

- make the interface-consuming Salsa query family the sole semantic design;
- fix import visibility, recovered environments, ADTs, patterns, methods,
  interfaces, generic constraints, coherence, external ABI, and entry/library
  behavior directly;
- use compiler-owned core environments and stable semantic identities;
- complete the semantic artifacts required by stable-provenance lowering;
- replace differential tests with language-level acceptance tests;
- preserve stable-ID invalidation boundaries so private body edits stop at
  equal module environments;
- avoid dependencies on flattened dependency ASTs, synthetic prelude spans, or
  compatibility symbol maps.

This workspace does not preserve compatibility-only diagnostic names, query
APIs, or internal result types.

## Workspace B: remove the legacy compiler

Workspace B owns deletion and simplification:

- remove flattened dependency AST assembly and span-offset provenance;
- remove `compat_*` queries and compatibility-only module-analysis wrappers;
- remove `SemanticPipeline`, differential projection helpers, and test-only
  compatibility selection;
- remove compatibility-only tests that assert implementation details or exact
  parity with the deleted pipeline;
- retain public compiler facades where they remain useful;
- retain language, CLI, LSP, diagnostics, generated-JavaScript, runtime, and
  golden behavior tests as the acceptance suite for Workspace A;
- leave small explicit handoff points when rewiring requires replacement code
  that belongs to Workspace A rather than introducing temporary abstractions.

Workspace B must not delete tests merely because the new compiler currently
fails them. A test is removable only when it verifies a deleted internal seam;
tests of language or runtime behavior remain.

## Compatibility policy

Compiler-internal breaking changes are allowed. The following may change:

- internal Rust APIs and type layouts;
- query names and test-only inspection APIs;
- incidental diagnostic wording or ordering where semantics are unchanged;
- generated formatting that does not alter JavaScript behavior;
- compatibility-only mangled names and synthetic spans.

The following remain stable:

- language syntax and typing rules;
- visibility and import semantics;
- interface, implementation, coherence, and dispatch semantics;
- entry-point and library-mode rules;
- standard-library behavior and external ABI semantics;
- generated program behavior;
- compiler-owned core identity and runtime ownership;
- recovered dependency cascade suppression and refusal to lower poisoned state.

## Merge and verification

Merge Workspace B into Workspace A after both have clean focused checks.
Resolve conflicts in favor of the new query and semantic data flow. After the
merge:

1. remove residual compatibility names, adapters, and dead feature flags;
2. run formatting and static checks;
3. run all retained sema, compiler, codegen, CLI, LSP, and standard-library
   tests;
4. inspect every removed or changed golden assertion to ensure it represented
   an internal detail rather than a language behavior;
5. run incremental invalidation tests for unchanged, private-body, public-shape,
   and recovered-dependency edits;
6. run representative generated JavaScript under Node;
7. measure clean and warm performance against the recorded baseline.

The merge is complete only when the repository contains one semantic pipeline
and no production or test code can select the flattened compiler.

## Concurrency and parallelism follow-up

Concurrency work begins only after the sole incremental compiler is green and
profiled. The investigation will use Criterion timings, Salsa `WillExecute`
events, phase counters, and CPU profiles to rank opportunities. Candidate areas
include independent module parsing, dependency-ready module checking, runtime
artifact lowering, module emission, and test/build orchestration.

Parallelism will be introduced only where dependency ordering and Salsa's
database access model make it safe. Determinism, diagnostic ordering, stable ID
allocation, and cache effectiveness are required invariants. Every accepted
parallel change must show a measured speedup on wide or mixed graphs and must
not regress deep-graph or small-project performance materially.

## Tracker changes

- #78 becomes completion of the sole interface-consuming semantic compiler,
  not a permanent differential seam.
- #79 remains stable per-definition runtime artifacts and lowering, implemented
  only for the new semantic compiler.
- #80 becomes merge cleanup, language-level verification, performance gates,
  and removal of any residual legacy code rather than an atomic pipeline
  selector cutover.
- A new follow-up issue will track profiled concurrency and parallelism work
  after #80 is green.
