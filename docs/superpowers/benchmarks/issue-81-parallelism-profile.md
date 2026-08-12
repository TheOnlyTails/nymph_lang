# Issue #81 compiler parallelism profile

Captured 2026-08-12 at exact base
`08fa157f4c9189c098f13de801e55ea83199925b`.

## Decision

Keep the existing native ambient-first, module-level diagnostics prewarmer and
make **no compiler concurrency change**. It already provides the only material,
repeatable end-to-end win found: at four Rayon workers, clean diagnostics are
20.4–33.7% faster than at one worker across small, wide, deep, and mixed
fixtures. Increasing the pool from four workers (the physical-core count) to
eight logical workers produced no statistically repeatable improvement, raised
whole-process peak RSS by 9.1 MiB (2.9%), and raised total CPU consumption while
occupancy remained below 1.5 cores. Warm requests execute no Salsa queries and
complete in 3.0–5.1 µs, so they must not acquire more parallel work.

The remaining clean-build regression reported by #80 is not explained by
missing graph concurrency. Diagnostics account for nearly the entire current
full-build time. The dominant critical path is the construction of each
module's semantic environment from ambient and dependency interfaces followed
by `check_module_with_owned_environment`; dependent module interfaces serialize
the deep path. `perf`, Callgrind, DHAT, and allocation profilers were unavailable
in the orb, so this report does not claim an allocation-site split between
environment construction, canonical fact instantiation, and checker work.

The checked-in benchmark expansion is retained because it makes the
small/wide/deep/mixed cold and warm safeguards reproducible. No speculative
runtime code or concurrency-specific tests are included because no new
concurrency boundary passed the evidence gate.

## Controlled environment and methodology

```text
Host: Amp Linux orb, x86_64
CPU: Intel Xeon, 4 physical cores / 8 logical CPUs
rustc: 1.99.0-nightly (3d6c19bb9 2026-08-11)
cargo: 1.99.0-nightly (b07e5a086 2026-08-07)
Node: v24.19.0
Criterion: 0.7.0, plotters backend
Base: 08fa157f4c9189c098f13de801e55ea83199925b
Build controls: CARGO_BUILD_JOBS=2, CARGO_INCREMENTAL=0
Runtime control: RAYON_NUM_THREADS=1,2,4,8
Samples: 10; warm-up: 1 s; requested measurement: 2 s
```

The clean shape sweep used the exact stateless facade and generated fixtures
from `GraphShape`. Fixture construction was outside Criterion's timed closure.
The warm sweep used one retained `CompilerSession`, stable project/module keys,
and primed the exact request during untimed setup. Criterion extended the
requested measurement period to collect ten heavyweight iterations. Each
reported interval is Criterion's 95% confidence interval and middle point
estimate.

Representative reproduction commands:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo bench -p nymph-compiler \
  --features test-support --bench incremental_project --no-run

RAYON_NUM_THREADS=4 /usr/bin/time \
  -f 'PROCESS user_s=%U sys_s=%S wall_s=%e cpu=%P max_rss_kb=%M' \
  target/release/build/nymph-compiler/*/out/incremental_project-* \
  --bench \
  'baseline-compatible/(single|wide-16|deep-16|mixed-4x4)/(diagnostics|full-compile)$' \
  --sample-size 10 --warm-up-time 1 --measurement-time 2 --noplot

RAYON_NUM_THREADS=4 target/release/build/nymph-compiler/*/out/incremental_project-* \
  --bench \
  'profile-shapes/(single|wide-16|deep-16|mixed-4x4)/(diagnostics|full-compile)/(fresh|warm)$' \
  --sample-size 10 --warm-up-time 1 --measurement-time 2 --noplot
```

`/usr/bin/time` covered the benchmark preflight and every selected case, so its
CPU and RSS numbers are comparable thread-count observations, not per-operation
attribution. RSS is the whole-process maximum. Criterion timing excludes setup,
source installation, priming, source mutation, and state destruction.

## Raw clean shape sweep

### One Rayon worker

| Shape | Diagnostics | Full compile |
|---|---:|---:|
| Single | 548.02 ms (544.80–551.15) | 570.11 ms (562.33–579.92) |
| Wide 16 | 712.80 ms (702.08–723.81) | 714.19 ms (703.25–725.69) |
| Deep 16 | 713.49 ms (701.39–727.30) | 712.40 ms (696.71–734.68) |
| Mixed 4×4 | 690.55 ms (680.92–701.64) | 707.19 ms (696.97–719.56) |

Process totals: user 74.63 s, system 0.26 s, wall 74.92 s, 99% CPU,
304,516 KiB maximum RSS.

### Two Rayon workers

| Shape | Diagnostics | Full compile |
|---|---:|---:|
| Single | 399.47 ms (397.53–401.73) | 408.29 ms (405.65–411.34) |
| Wide 16 | 503.06 ms (502.04–504.10) | 531.73 ms (522.15–543.90) |
| Deep 16 | 560.32 ms (552.68–568.83) | 582.30 ms (574.63–591.97) |
| Mixed 4×4 | 508.53 ms (502.66–516.72) | 521.74 ms (515.44–532.15) |

Process totals: user 74.84 s, system 0.46 s, wall 56.30 s, 133% CPU,
313,184 KiB maximum RSS.

### Four Rayon workers

| Shape | Diagnostics | Full compile |
|---|---:|---:|
| Single | 384.14 ms (378.98–389.84) | 398.82 ms (389.76–408.68) |
| Wide 16 | 472.93 ms (465.68–484.30) | 485.69 ms (478.98–493.53) |
| Deep 16 | 568.27 ms (557.58–578.31) | 564.01 ms (556.23–572.74) |
| Mixed 4×4 | 491.30 ms (473.89–510.02) | 503.05 ms (487.24–523.05) |

Process totals: user 77.45 s, system 0.55 s, wall 53.86 s, 144% CPU,
319,728 KiB maximum RSS.

### Eight Rayon workers

| Shape | Diagnostics | Full compile |
|---|---:|---:|
| Single | 393.51 ms (389.57–397.66) | 396.92 ms (387.62–409.18) |
| Wide 16 | 463.96 ms (460.19–468.56) | 485.26 ms (476.98–493.93) |
| Deep 16 | 565.13 ms (552.56–579.52) | 556.52 ms (551.45–561.78) |
| Mixed 4×4 | 488.21 ms (474.80–503.03) | 492.18 ms (483.41–502.31) |

Process totals: user 78.79 s, system 0.88 s, wall 53.54 s, 148% CPU,
329,576 KiB maximum RSS.

Four workers reduced diagnostics relative to one worker by 29.9% for Single,
33.7% for Wide 16, 20.4% for Deep 16, and 28.9% for Mixed 4×4. Four-to-eight
confidence intervals overlap for every full compile and all but the small
diagnostics case; small diagnostics regressed 2.4%. The follow-up raw Mixed
diagnostics samples likewise averaged 470.4 ms at four workers and 478.3 ms at
eight workers, a 1.7% regression rather than a gain.

### Raw repeated Mixed 4×4 samples

Values are milliseconds per operation calculated directly from Criterion's
`sample.json` `times / iters`, in collection order:

```text
1 worker diagnostics:
679.051 697.690 675.238 671.005 679.182 672.794 685.664 706.809 708.237 711.763
1 worker full compile:
697.176 697.276 694.060 691.515 698.108 695.170 698.661 690.348 695.634 694.169

4 workers diagnostics:
476.667 464.253 477.519 472.589 461.473 472.244 477.565 461.875 464.811 474.829
4 workers full compile:
492.476 514.353 486.650 501.819 503.954 484.050 476.414 477.087 467.156 487.926

8 workers diagnostics:
455.319 470.778 460.020 473.894 460.994 501.738 493.290 466.694 502.439 497.353
8 workers full compile:
518.618 481.791 488.941 475.379 513.710 475.937 474.174 475.274 474.418 491.050
```

## Raw retained-session shape sweep

These four-worker cases share one process and pair fresh and exactly primed
requests. Warm setup is untimed.

| Shape | Fresh diagnostics | Warm diagnostics | Fresh full compile | Warm full compile |
|---|---:|---:|---:|---:|
| Single | 381.00 ms (371.28–394.70) | 3.6577 µs (3.3586–4.0219) | 385.08 ms (377.55–393.59) | 4.7953 µs (4.5015–5.0725) |
| Wide 16 | 458.44 ms (432.57–489.00) | 3.3702 µs (3.0155–3.7617) | 457.66 ms (447.73–467.41) | 5.0984 µs (4.6051–5.6265) |
| Deep 16 | 490.82 ms (483.37–498.42) | 3.5631 µs (3.3066–3.7939) | 555.88 ms (531.66–581.79) | 4.7439 µs (4.0370–5.5550) |
| Mixed 4×4 | 440.79 ms (428.48–457.56) | 2.9869 µs (2.8348–3.1436) | 460.39 ms (452.18–467.94) | 4.2245 µs (3.8676–4.6057) |

The event audit observed zero query executions for repeated diagnostics and
analysis requests. Existing shape invalidation tests compile and run all three
17-module graphs under Node, then prove an identical full compile executes no
queries and a private leaf edit reruns analysis/emission only for that leaf plus
exactly one stable `runtime_definition` and `lower_runtime_definition`. A public
signature edit rechecks exactly `api`, `direct`, `transitive`, and `main`, not
the two installed unrelated modules.

## Phase attribution and critical paths

A separate four-worker retained Mixed 4×4 run measured the existing public
phase boundaries:

| Inclusive boundary | Wall estimate | Increment over prior boundary |
|---|---:|---:|
| diagnostics | 458.29 ms (445.27–470.36) | — |
| prebundle emitted project | 469.42 ms (452.92–486.18) | about 11 ms |
| full compile | 506.05 ms (488.10–524.64) | about 37 ms |

Confidence intervals overlap, so the subtractions are attribution estimates,
not independent Criterion distributions. The exact baseline-compatible sweep
usually placed full compile only 8–21 ms above diagnostics. Either way,
diagnostics contribute roughly 91–97% of end-to-end cold latency; per-definition
lowering, module emission, and bundling cannot explain the #80 regression.

### Parse and graph

Project graph discovery is a deterministic recursive DFS. It invokes one Salsa
parse query per reachable module to discover imports, then appends modules in
dependency-first postorder. Therefore parsing and graph discovery cannot be
cleanly split by the public request boundary without changing demand order.
Sampling profilers were unavailable, so no unsupported wall-time split is
reported. Event evidence establishes the work cardinality instead: every
17-module fixture performs one project parse per reachable module, and the
canonical ambient registry retains 12 distinct Salsa parse queries while
reusing immutable canonical parse values across sessions.

Explicitly prewarming all installed parses was rejected: reachability is not
known until imports are parsed, so it would parse unreachable installed modules,
increase simultaneous AST retention, and add no warm benefit. The existing
semantic prewarmer already overlaps project parsing on independent branches.

### Dependency-ready semantics

For each module, `interface_module_analysis` obtains every dependency's closed
`ModuleEnvironment`, combines those interfaces with ambient roots in
`SemanticEnvironment::from_modules_with_runtime_roles`, installs deterministic
module tags/import bindings, then invokes
`check_module_with_owned_environment`. Dependents cannot complete until the
dependency interface is extracted, which is the deep-graph critical path.

The current native prewarmer uses one cloned Salsa storage handle per task,
warms ambient roots first, then requests all project module diagnostics in a
private Rayon pool. Cloned storage shares memo tables but has a distinct local
query stack. A coordinator thread prevents the aggregate-query caller from
switching database handles while its Salsa stack is active. The authoritative
caller then folds memoized child diagnostics serially in graph order, registering
aggregate dependencies and preserving diagnostic order. Wasm retains this
serial fold without native prewarming.

The thread sweep shows this boundary already exposes available branch width.
The whole run reached only 144–148% CPU with four to eight workers, indicating
dependency waits/Salsa synchronization and serial semantic work rather than a
lack of runnable Rayon tasks. Explicit topological layer barriers were rejected:
Salsa demand already blocks only consumers whose dependencies are unavailable,
whereas a whole-layer barrier would delay ready work behind unrelated long
branches and add pure overhead to deep graphs.

### Per-definition lowering

`runtime_definition` and `lower_runtime_definition` are already memoized by
stable `DefinitionId`. Module lowering consumes a source-ordered FIFO demand
closure: each fragment may discover host-runtime dependencies, direct demands,
and exact iterator implementations. Completion order therefore cannot replace
queue order without risking stable attachment/initializer order.

A parallel-frontier prewarm followed by authoritative serial consumption would
respect Salsa, but it was rejected without implementation: diagnostics leave at
most 2–9% of end-to-end time for all lowering, emission, and bundling combined;
incremental private edits lower exactly one definition; and retaining an entire
frontier raises transient allocation pressure. There is no material end-to-end
ceiling on the measured fixtures.

### Module emission and bundling

`emitted_interface_module` is a plausible isolated tracked query. It could be
prewarmed through cloned databases, then consumed serially in semantic order so
parallel completion never determines source-map insertion, virtual-fragment
deduplication, entry tags, imports, exports, or JavaScript. It was rejected for
the same measured ceiling: prebundle work added about 11 ms in the phase run,
and exact clean pairs usually differed by less than the confidence intervals.
Concurrent emission would also increase the instantaneous live set of stable
HIR modules and generated strings in an allocation-sensitive pipeline.

Bundling is one project-wide Rolldown invocation on a current-thread Tokio
runtime. There is no compiler-level per-module bundle boundary. Parallelizing
independent compilation requests is an outer throughput policy and would
oversubscribe the existing private diagnostics pool.

### Node verification

Node is external to compiler latency. Shape invalidation tests execute each
fresh bundle under Node and compare stdout; the full test runner already
parallelizes independent test processes. Adding internal Node fan-out would
measure test throughput, not compilation, and risks CPU/memory oversubscription.
Node verification therefore remains serial inside each test and controlled by
the outer nextest concurrency policy.

## #80 cold-regression and allocation investigation

#80's same-orb sole-pipeline comparison measured 498.07 ms diagnostics and
501.23 ms full compile versus 120.23 ms and 136.78 ms at historical
`abcbd4bd5b992cb29536d1b81623a41a888da6e4`: 4.14× and 3.66×. The exact Salsa
audit found one `interface_module_analysis` and one `emitted_interface_module`
per reachable project module and no warm executions, ruling out duplicate
project analysis, duplicate emission, and accidental warm invalidation.

The #80 review also repeated the current benchmark with the historical
process-global counting allocator. Ordinary current diagnostics/full compile
were 460.34/477.14 ms; atomic accounting on every allocation, reallocation, and
deallocation raised them to 868.81/843.16 ms. The historical executable used
the same accounting yet measured 120.23/136.78 ms. Instrumentation therefore
does not cause the regression: it demonstrates that the current diagnostics
path performs substantially more allocation-sensitive ownership work.

This orb had `/usr/bin/time` but no `perf`, Valgrind/Callgrind, DHAT, heaptrack,
Samply, or cargo-flamegraph. Whole-process RSS rose from 304,516 KiB at one
worker to 319,728 KiB at four and 329,576 KiB at eight. Because no allocation
profiler was available, the remaining regression is attributed only to the
narrowest supported boundary: environment/fact construction plus checking
inside diagnostics. Allocation-site optimization should precede any additional
parallel scheduling experiment.

## Evaluated boundaries

| Boundary | Decision | Evidence |
|---|---|---|
| Existing ambient-first independent module diagnostics | **Keep** | 20.4–33.7% clean wall win at four versus one worker; deterministic serial fold and warm backdating already tested. |
| No additional concurrency | **Chosen** | Best end-to-end result after safeguards; four-to-eight scaling is flat, CPU occupancy is low, and RSS rises. |
| Explicit dependency-ready semantic layers | Reject | Salsa already schedules demand; barriers reduce overlap and deep graphs have width one. |
| Independent parse prewarm | Reject | Reachability requires parsing; all-active prewarm does unnecessary work and retains more ASTs. |
| Per-definition lowering frontiers | Reject | Dynamic FIFO demand order is semantic; incremental edits have one item; total post-diagnostic ceiling is too small. |
| Per-module emission prewarm | Reject | Safe only with cloned DBs plus serial fold, but measured prebundle ceiling is about 11 ms and memory pressure rises. |
| Compiler-level bundling parallelism | Reject | One opaque project-wide Rolldown operation; no independent compiler boundary. |
| Internal Node verification fan-out | Reject | Outside compiler latency and already governed by nextest; oversubscription risk. |

## Limitations

- Results are from one shared virtualized orb and should be compared as paired
  same-process ratios, not absolute cross-machine baselines.
- Criterion used ten samples and short warm-up/measurement targets to keep the
  full shape/thread matrix tractable; heavyweight cases extended the collection
  period automatically.
- `/usr/bin/time` cannot attribute RSS or CPU occupancy to individual compiler
  phases, and its totals include the benchmark's invariant audit.
- Parse and graph wall time cannot be independently observed through the public
  pipeline because graph discovery consumes parse results. The report records
  their query cardinality and critical path rather than inventing a subtraction.
- No allocation-site, lock-contention, instruction, cache-miss, or sampled-stack
  profile was available. The environment/checker split within diagnostics
  remains the next profiling target on a machine with `perf` or an allocation
  profiler.
