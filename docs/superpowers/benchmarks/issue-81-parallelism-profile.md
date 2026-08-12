# Issue #81 compiler parallelism acceptance evidence

Captured 2026-08-12 from exact remote `issue-81-agent` tip
`269f87c963631f21ad53677c21e624e98f91f4f7` plus the evidence-only changes
described here.

## Decision

Make **no new compiler concurrency change**. On this 4-core/8-thread orb, four
workers are the best measured tradeoff, not a new hard-coded production pool
size: the existing Rayon-selected policy remains platform-dependent. Five
independent fresh-process repeats for every cell show four workers reduce
uninstrumented cold diagnostics by
27.2–32.6% and cold full compile by 26.6–34.1% versus one worker. Eight workers
improve only the Single fixture by about 3.8%; they are flat on Deep and regress
Wide/Mixed full compile by 7.3%/5.3%. The maximum observed process RSS rises
from 94,792 KiB at four workers to 106,180 KiB at eight (+12.0%), while median
process CPU over all uninstrumented cells rises from 1.11 s to 1.18 s.

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

`/usr/bin/time` records process user CPU, system CPU, wall time, and peak RSS.
Its process measurements include the cold request and the subsequent 200 ms
warm loop; `cold_wall_ns` and `warm_total_ns` separately report compiler request
walls. RSS is a whole-process maximum, not phase attribution.

Checked-in files under `issue-81-data/`:

- `raw.jsonl`: every matrix observation and phase/counter payload;
- `snapshots.jsonl`: all 48 exact snapshot payloads and canonical SHA-256s;
- `summary.json`: deterministic statistics generated only from `raw.jsonl`;
- `environment.json`: OS, architecture, Rust/Cargo/Node/Python, capture
  checkout, and controls;
- `commands.txt`: exact build and execution templates.
- `artifacts.sha256`: SHA-256 integrity manifest for all five files above.

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
| 1 | Single | 693.10 ms | 0.157 µs | 716.99 ms | 0.202 µs |
| 1 | Wide 16 | 845.34 ms | 0.158 µs | 835.83 ms | 0.193 µs |
| 1 | Deep 16 | 818.94 ms | 0.153 µs | 822.62 ms | 0.193 µs |
| 1 | Mixed 4×4 | 830.28 ms | 0.157 µs | 827.53 ms | 0.190 µs |
| 2 | Single | 483.54 ms | 0.156 µs | 502.13 ms | 0.195 µs |
| 2 | Wide 16 | 599.53 ms | 0.153 µs | 615.24 ms | 0.200 µs |
| 2 | Deep 16 | 626.87 ms | 0.155 µs | 644.30 ms | 0.203 µs |
| 2 | Mixed 4×4 | 598.81 ms | 0.155 µs | 623.56 ms | 0.190 µs |
| 4 | Single | 466.91 ms | 0.155 µs | 472.52 ms | 0.194 µs |
| 4 | Wide 16 | 571.07 ms | 0.155 µs | 580.00 ms | 0.195 µs |
| 4 | Deep 16 | 596.09 ms | 0.154 µs | 603.95 ms | 0.197 µs |
| 4 | Mixed 4×4 | 571.16 ms | 0.153 µs | 560.10 ms | 0.191 µs |
| 8 | Single | 448.73 ms | 0.153 µs | 454.79 ms | 0.189 µs |
| 8 | Wide 16 | 576.00 ms | 0.152 µs | 622.36 ms | 0.191 µs |
| 8 | Deep 16 | 599.98 ms | 0.154 µs | 601.33 ms | 0.193 µs |
| 8 | Mixed 4×4 | 579.71 ms | 0.157 µs | 589.75 ms | 0.194 µs |

The summarizer retains mean, median, minimum, maximum, maximum RSS, median
process CPU, phase medians, and counts for every cell. The table is intentionally
not presented as a confidence interval: five repeats establish repeatability
and preserve raw values, but this one virtualized orb is not a population study.

## Timing instrumentation and overlap

Instrumentation exists only behind `test-support` and is enabled explicitly by
the evidence executable. Ordinary builds compile no timing branches or atomics.
The following is the four-worker Mixed median from five instrumented processes:

| Inclusive counter | Diagnostics (count) | Full compile (count) |
| ----------------- | ------------------: | -------------------: |
| Outer request wall | 579.03 ms (1) | 595.79 ms (1) |
| Parse | 172.72 ms (29) | 176.57 ms (29) |
| Interface/environment construction | 68.07 ms (17) | 89.89 ms (17) |
| Checker | 182.36 ms (17) | 265.24 ms (17) |
| Diagnostic fold/wrapper | 0.095 ms (18) | 0.112 ms (18) |
| Stable lowering | 0 ms (0) | 6.38 ms (17) |
| Module emission | 0 ms (0) | 0.236 ms (17) |
| Bundling | 0 ms (0) | 5.16 ms (1) |

Each phase records inclusive elapsed wall for every execution. Parallel query
spans overlap one another, parse can overlap semantic work, stable lowering is
nested beneath module emission, and the outer wall includes Salsa scheduling and
all uninstrumented wrapper work. Therefore phase totals are work/span evidence,
not additive attribution, CPU time, or percentages of the outer wall. The 18
diagnostic executions are 17 module wrappers plus the authoritative serial
project fold.

Adjacent uninstrumented/instrumented fresh processes quantify counter overhead.
Across 160 pairs, the cold-wall ratio `(on/off - 1)` has mean −0.51% and median
−0.36%; the 32 cell medians range from −5.11% to +7.16%. The signal is smaller
than process noise, so this evidence finds no measurable systematic overhead;
it does not claim the counters are free.

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

## Determinism gate

The snapshot runner starts a new process for every worker/shape/repeat cell and
compares the entire canonical payload, failing on any mismatch. The payload
contains exact sorted diagnostics, dependency-first compiler graph order,
sorted stable `DefinitionId` inventory, exact emitted module-source mappings,
the final bundled JavaScript BLAKE3, and Node stdout. All 48 snapshots (4 worker
counts × 4 shapes × 3 repeats) matched exactly.

| Shape | Canonical snapshot SHA-256 | Final JS BLAKE3 | IDs/modules | Node stdout |
| ----- | ------------------------- | --------------- | ----------: | ----------- |
| Single | `a52f7497137a0f5524b16dae1ce92ba5495be9f1a8f1c3481fcb60a949bdf810` | `f78c5eff032107792c5b1889b55669556356578f5146d1e69b3056adb66babfd` | 1/1 | `NInt { v: 0 }` |
| Wide 16 | `dab76be0505dd3eb7f7fb678df8deac0f61b37a3330c88820f668bb25de1c7e0` | `fd30378d56e2102a8601d3d6547d603ad10405a91807a66a9d23a2cd2c50a43a` | 17/17 | `NInt { v: 0 }` |
| Deep 16 | `f0e2072c8d554349eb49039d513766e470061f3fc03f5c11d3a87c17be3d9a62` | `fd30378d56e2102a8601d3d6547d603ad10405a91807a66a9d23a2cd2c50a43a` | 17/17 | `NInt { v: 0 }` |
| Mixed 4×4 | `2f68d47bb2320891e24a3a011f6349120e1e845d847a19163c79800bf6ab4f4e` | `fd30378d56e2102a8601d3d6547d603ad10405a91807a66a9d23a2cd2c50a43a` | 17/17 | `NInt { v: 0 }` |

Wide, Deep, and Mixed bundle to the same final JS because their imported leaf
values are unused and Rolldown removes them; their graph orders, ID inventories,
and exact module-source payloads remain different and are compared.

## Boundaries considered

| Boundary | Decision | Evidence |
| -------- | -------- | -------- |
| Existing ambient-first module diagnostics prewarm | **Keep; do not hard-code 4** | Four is this orb's best tradeoff and gives a 26.6–34.1% full-compile win over one worker; exact output and active bound pass. One host is insufficient to replace Rayon's platform policy. |
| Expand diagnostics pool to 8 | Reject | Flat/regressed multi-module walls, +12.0% maximum RSS, higher process CPU. |
| Parallel parse prewarm | Reject | Reachability itself consumes parse results; risks parsing unreachable installed modules. |
| Explicit semantic layer barriers | Reject | Salsa demand already waits at dependency edges; Deep has no useful layer width. |
| Parallel stable-lowering frontier | Reject | FIFO discovered-demand order is semantic and measured 4-worker Mixed inclusive work is 6.38 ms. |
| Parallel module emission | Reject | Measured inclusive work is 0.236 ms and deterministic serial assembly remains required. |
| Compiler-level bundling fan-out | Reject | One project-wide Rolldown invocation; 5.16 ms median here. |
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
- Exact diagnostics are compared but are empty for these successful fixtures;
  compiler diagnostic-order behavior remains covered by the existing dedicated
  error-fixture tests.
- The data supports the bounded decision for these fixtures and this machine;
  it does not prove eight workers can never help another workload.
- The worker sweep is sensitivity analysis, not evidence for a portable fixed
  pool size. Production continues to use Rayon's platform-selected default.
