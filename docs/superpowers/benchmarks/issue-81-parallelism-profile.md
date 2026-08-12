# Issue #81 compiler parallelism acceptance evidence

Captured 2026-08-12 from exact remote `issue-81-agent` tip
`269f87c963631f21ad53677c21e624e98f91f4f7` plus the evidence-only changes
described here.

## Decision

Make **no new compiler concurrency boundary**. On this 4-core/8-thread orb,
four workers are the best measured tradeoff, not a hard-coded production pool
size. The existing pool now prevents explicit oversubscription beyond available
parallelism. Five independent fresh-process repeats for
every cell show four workers reduce uninstrumented cold diagnostics by
27.1–35.7% and cold full compile by 25.0–35.7% versus one worker. Eight workers
are within ±3.0% except Wide compile, which regresses 7.4%. The maximum observed
cold-operation process RSS rises from 93,500 KiB at four workers to 104,892 KiB
at eight (+12.2%).

The corrected evidence does not support the previous report's exact Criterion
confidence-interval claims because those raw samples were not retained. This
package instead checks in every fresh-process observation, a deterministic
summarizer, exact environment and commands, phase counters, a concurrency-bound
test, and cross-worker snapshots. The decision remains no new concurrency; the
four-worker result is a measured platform point rather than a portable
configuration claim.

## Reproduction and retained artifacts

```sh
CARGO_INCREMENTAL=0 python3 scripts/issue-81-evidence.py matrix --repeats 5
python3 scripts/issue-81-evidence.py snapshots --repeats 3
python3 scripts/issue-81-evidence.py bound
python3 scripts/issue-81-evidence.py summarize
python3 scripts/issue-81-evidence.py verify
```

The matrix contains 320 process rows: 4 worker counts × 4 shapes × 2 requests ×
5 repeats × instrumented/uninstrumented. Each `sample` invocation is a new
process with `RAYON_NUM_THREADS` set before the private pool's `OnceLock` is
initialized. Source installation is outside the cold request timer. A cold
diagnostics or full-compile request is followed by exact memoized requests for
at least 200 ms and at least 10,000 iterations; observed iteration counts were
835,000–1,328,000, enough to resolve sub-microsecond medians. Diagnostics and
compile are separate processes, so neither cold measurement primes the other.

`/usr/bin/time` records whole-process user CPU, system CPU, wall time, and peak
RSS, including the subsequent warm loop. Separately, `/proc/self/stat` CPU ticks
and `/proc/self/status` RSS/HWM are sampled immediately around only the cold
request; these uncontaminated cold-operation values drive CPU occupancy and RSS
claims. RSS remains a process maximum since launch, not phase attribution.

Checked-in files under `issue-81-data/`:

- `raw.jsonl`: every matrix observation and phase/counter payload;
- `snapshots.jsonl`: all 48 exact snapshot payloads and canonical SHA-256s;
- `summary.json`: deterministic statistics generated only from `raw.jsonl`;
- `bound.json`: requested, configured, and observed worker counts from an
  explicit oversubscription attempt;
- `environment.json`: OS, architecture, Rust/Cargo/Node/Python, requested base
  tip, evidence capture parent, and controls;
- `commands.txt`: exact build and execution templates.
- `artifacts.sha256`: SHA-256 integrity manifest for all six files above.

The retained environment records that the raw matrix collection ran with the
evidence source changes uncommitted atop
`269f87c963631f21ad53677c21e624e98f91f4f7`; those sources and the original
artifacts were committed as `e972e6375a21231a6b9b6dd4de940ccb792fbfc3`.
The final audit regenerated the snapshots after removing a non-semantic sorted
`module_order` field. This is sufficient to inspect the exact captured source,
but the raw matrix is not misrepresented as a clean-checkout run.

## Fresh-process results

Values below are medians of five **uninstrumented** cold repeats and the warm
per-iteration median from each process's long warm loop.

| Workers | Shape | Cold diagnostics | Warm diagnostics | Cold compile | Warm compile |
| ------: | ----- | ---------------: | ---------------: | -----------: | -----------: |
| 1 | Single | 683.43 ms | 0.155 µs | 693.07 ms | 0.192 µs |
| 1 | Wide 16 | 809.19 ms | 0.154 µs | 827.87 ms | 0.191 µs |
| 1 | Deep 16 | 806.01 ms | 0.157 µs | 817.74 ms | 0.194 µs |
| 1 | Mixed 4×4 | 791.16 ms | 0.157 µs | 806.99 ms | 0.199 µs |
| 2 | Single | 479.84 ms | 0.152 µs | 492.87 ms | 0.191 µs |
| 2 | Wide 16 | 590.05 ms | 0.155 µs | 611.45 ms | 0.195 µs |
| 2 | Deep 16 | 625.23 ms | 0.152 µs | 656.19 ms | 0.195 µs |
| 2 | Mixed 4×4 | 591.43 ms | 0.156 µs | 608.82 ms | 0.197 µs |
| 4 | Single | 439.24 ms | 0.152 µs | 445.50 ms | 0.192 µs |
| 4 | Wide 16 | 535.45 ms | 0.155 µs | 555.13 ms | 0.190 µs |
| 4 | Deep 16 | 587.32 ms | 0.154 µs | 613.53 ms | 0.191 µs |
| 4 | Mixed 4×4 | 564.88 ms | 0.159 µs | 548.28 ms | 0.196 µs |
| 8 | Single | 442.95 ms | 0.153 µs | 450.22 ms | 0.191 µs |
| 8 | Wide 16 | 549.42 ms | 0.154 µs | 596.38 ms | 0.193 µs |
| 8 | Deep 16 | 595.64 ms | 0.154 µs | 604.19 ms | 0.193 µs |
| 8 | Mixed 4×4 | 560.51 ms | 0.158 µs | 564.78 ms | 0.198 µs |

The summarizer retains mean, median, minimum, maximum, maximum RSS, median
process CPU, phase medians, and counts for every cell. The table is intentionally
not presented as a confidence interval: five repeats establish repeatability
and preserve raw values, but this one virtualized orb is not a population study.

The next table reports the median cold-operation CPU occupancy and maximum
cold-operation high-water RSS. Occupancy is `(user ticks + system ticks) /
CLK_TCK / cold wall`, using only the parent compiler process. It is therefore an
average utilized-core count over the intended request, not whole-process CPU or
a phase attribution.

| Workers | Shape | Diagnostics cores | Diagnostics RSS | Compile cores | Compile RSS |
| ------: | ----- | ----------------: | --------------: | ------------: | ----------: |
| 1 | Single | 0.99 | 40,220 KiB | 0.99 | 49,808 KiB |
| 1 | Wide 16 | 0.99 | 77,880 KiB | 0.99 | 83,936 KiB |
| 1 | Deep 16 | 0.99 | 77,212 KiB | 0.99 | 84,668 KiB |
| 1 | Mixed 4×4 | 0.99 | 77,072 KiB | 0.98 | 85,316 KiB |
| 2 | Single | 1.42 | 43,488 KiB | 1.40 | 53,400 KiB |
| 2 | Wide 16 | 1.41 | 80,780 KiB | 1.41 | 88,224 KiB |
| 2 | Deep 16 | 1.33 | 80,436 KiB | 1.31 | 88,344 KiB |
| 2 | Mixed 4×4 | 1.42 | 79,936 KiB | 1.41 | 88,260 KiB |
| 4 | Single | 1.57 | 47,064 KiB | 1.56 | 55,988 KiB |
| 4 | Wide 16 | 1.63 | 85,856 KiB | 1.64 | 93,500 KiB |
| 4 | Deep 16 | 1.44 | 85,456 KiB | 1.45 | 92,944 KiB |
| 4 | Mixed 4×4 | 1.59 | 85,288 KiB | 1.63 | 93,404 KiB |
| 8 | Single | 1.64 | 56,040 KiB | 1.62 | 65,856 KiB |
| 8 | Wide 16 | 1.72 | 96,552 KiB | 1.73 | 104,364 KiB |
| 8 | Deep 16 | 1.48 | 96,180 KiB | 1.49 | 103,968 KiB |
| 8 | Mixed 4×4 | 1.72 | 96,524 KiB | 1.71 | 104,892 KiB |

## Timing instrumentation and overlap

Instrumentation exists only behind `test-support` and is enabled explicitly by
the evidence executable. Ordinary builds compile no timing branches or atomics.
The following is the four-worker Mixed median from five instrumented processes:

| Inclusive counter | Diagnostics (count) | Full compile (count) |
| ----------------- | ------------------: | -------------------: |
| Outer request wall | 576.44 ms (1) | 584.42 ms (1) |
| Parse | 173.58 ms (29) | 172.81 ms (29) |
| Interface/environment construction | 69.24 ms (17) | 70.21 ms (17) |
| Checker | 147.50 ms (17) | 186.98 ms (17) |
| Diagnostic fold/wrapper | 0.085 ms (18) | 0.082 ms (18) |
| Stable lowering | 0 ms (0) | 5.64 ms (17) |
| Module emission | 0 ms (0) | 0.244 ms (17) |
| Bundling | 0 ms (0) | 5.23 ms (1) |

Each phase records inclusive elapsed wall for every execution. Parallel query
spans overlap one another, parse can overlap semantic work, stable lowering is
nested beneath module emission, and the outer wall includes Salsa scheduling and
all uninstrumented wrapper work. Therefore phase totals are work/span evidence,
not additive attribution, CPU time, or percentages of the outer wall. The 18
diagnostic executions are 17 module wrappers plus the authoritative serial
project fold.

Adjacent uninstrumented/instrumented fresh processes quantify counter overhead.
Across 160 pairs, the cold-wall ratio `(on/off - 1)` has mean +0.37% and median
−0.25%; individual noisy pairs range from −11.18% to +17.53%. The aggregate
signal is smaller than process noise, so this evidence finds no measurable
systematic overhead; it does not claim the counters are free.

The phase evidence reinforces the no-new-concurrency choice. Diagnostics are
the dominant request. Stable lowering, emission, and bundling together expose
only a small serial ceiling on these fixtures. Deep dependencies constrain
semantic width, while Wide/Mixed already exercise the existing diagnostics
prewarmer.

## Concurrency bound

The test-support-only active-task counter increments on entry to each ambient or
project prewarm task, atomically tracks the maximum, and decrements on exit. The
matrix fails immediately if maximum active work exceeds the private pool's
reported worker count. All 160 instrumented processes passed; observed maxima
were exactly 1, 2, 4, and 8 for their configured pools. The focused Rust test
also records both numbers and asserts `max_active <= configured_workers`.
The dedicated bound process requested 16 workers on an eight-CPU affinity set;
the pool configured eight and observed at most eight active tasks.

## Determinism gate

The snapshot runner starts a new process for every worker/shape/repeat cell and
compares the entire canonical payload, failing on any mismatch. The payload
contains exact sorted diagnostics, dependency-first compiler graph order,
sorted stable `DefinitionId` inventory, exact emitted module-source mappings,
the final bundled JavaScript BLAKE3, and Node stdout. All 48 snapshots (4 worker
counts × 4 shapes × 3 repeats) matched exactly, including three ordered non-empty
type diagnostics (`left`, `right`, `main`).

| Shape | Canonical snapshot SHA-256 | Final JS BLAKE3 | IDs/modules | Node stdout |
| ----- | ------------------------- | --------------- | ----------: | ----------- |
| Single | `c94080b0742cecf5eceb27d943cde7bd5c90d53bd56926d15b7b7084429a7b51` | `f78c5eff032107792c5b1889b55669556356578f5146d1e69b3056adb66babfd` | 1/1 | `NInt { v: 0 }` |
| Wide 16 | `1f298d6029a236166043de81ff2c0809ebebf783f2cb0ed8f9bd0f038493b84d` | `fd30378d56e2102a8601d3d6547d603ad10405a91807a66a9d23a2cd2c50a43a` | 17/17 | `NInt { v: 0 }` |
| Deep 16 | `1da63e20499823acf7893ba46c3d8d55cccbf637168d8f83f0899aced647f695` | `fd30378d56e2102a8601d3d6547d603ad10405a91807a66a9d23a2cd2c50a43a` | 17/17 | `NInt { v: 0 }` |
| Mixed 4×4 | `b86184281bc75ba7d8f77eb1252540d114a03d621283cb2d7d9b9e925c296b25` | `fd30378d56e2102a8601d3d6547d603ad10405a91807a66a9d23a2cd2c50a43a` | 17/17 | `NInt { v: 0 }` |

Wide, Deep, and Mixed bundle to the same final JS because their imported leaf
values are unused and Rolldown removes them; their graph orders, ID inventories,
and exact module-source payloads remain different and are compared.

## Boundaries considered

| Boundary | Decision | Evidence |
| -------- | -------- | -------- |
| Existing ambient-first module diagnostics prewarm | **Keep; do not hard-code 4** | Four is this orb's best tradeoff and gives a 25.0–35.7% full-compile win over one worker; exact output and active bound pass. One host is insufficient to replace the platform policy. |
| Expand diagnostics pool to 8 | Reject | Flat/regressed multi-module walls and +12.2% maximum RSS. |
| Parallel parse prewarm | Reject | Reachability itself consumes parse results; risks parsing unreachable installed modules. |
| Explicit semantic layer barriers | Reject | Salsa demand already waits at dependency edges; Deep has no useful layer width. |
| Parallel stable-lowering frontier | Reject | FIFO discovered-demand order is semantic and measured 4-worker Mixed inclusive work is 5.64 ms. |
| Parallel module emission | Reject | Measured inclusive work is 0.244 ms and deterministic serial assembly remains required. |
| Compiler-level bundling fan-out | Reject | One project-wide Rolldown invocation; 5.23 ms median here. |
| Internal Node fan-out | Reject | Node is verification outside compiler latency and outer test scheduling owns throughput. |

## Limitations

- Results describe one Amp orb on one date; use ratios and raw repeats rather
  than transporting absolute latency to other machines.
- `/usr/bin/time` reports whole-process CPU and peak RSS and cannot attribute
  either to phases. Warm-loop work is included in those process totals.
- Inclusive counters overlap and intentionally do not claim exclusive time.
- The benchmark does not use an allocation, lock-contention, cache-miss,
  instruction, or sampled-stack profiler. No allocation-site or lock attribution
  is claimed.
- Successful-fixture diagnostics and a dedicated non-empty ordered diagnostic
  fixture are compared in every cross-process snapshot.
- The data supports the bounded decision for these fixtures and this machine;
  it does not prove eight workers can never help another workload.
- The worker sweep is sensitivity analysis, not evidence for a portable fixed
  pool size. Production continues to use Rayon's platform-selected default.
