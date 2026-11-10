# Qubit Thread Pool Performance

[中文版](performance.zh_CN.md) · Current benchmark instructions apply to version 0.12.1; recorded measurements retain their original source baselines.

## Current benchmark groups

The source of truth is `benches/thread_pool_bench.rs`. Run all groups with
`cargo bench --bench thread_pool_bench --locked`, or select a group:

```bash
cargo bench --bench thread_pool_bench --locked -- thread_pool_steady_state
cargo bench --bench thread_pool_bench --locked -- thread_pool_end_to_end
cargo bench --bench thread_pool_bench --locked -- thread_pool_drop_request
```

| Group | Timed work | Outside timing / interpretation |
| --- | --- | --- |
| `thread_pool_steady_state` | Submit a batch and wait for task results/completion. | Pools are created outside iterations; dynamic workers are prestarted; shutdown is outside timing. |
| `thread_pool_end_to_end` | Qubit dynamic/fixed construction, submission, completion, shutdown and `wait_termination`. | Includes Qubit lifecycle termination; dynamic workers start lazily. |
| `thread_pool_drop_request` | External `threadpool` / Rayon construction, task completion and dropping the pool. | Does not observe worker termination; not equivalent to Qubit end-to-end timing. |
| `thread_pool_granularity` | Dynamic end-to-end batches at different task granularities. | Nominal total work is 2,048,000 iterations; per-task costs vary deterministically. |
| `thread_pool_idle_wakeup` | One no-op submission through its completion signal on one prestarted idle worker. | Construction and returning the worker to idle are excluded. |

The first three groups use 2,000 tasks, a base of 256 inner iterations, and
1/4/8 workers. Task costs use a deterministic bell-shaped distribution and
`black_box`. Steady-state Qubit cases collect callable results via handles;
external `threadpool` uses a channel, and Rayon uses parallel iteration and
reduction. These are application-path comparisons, not isolated queue costs.
Default Criterion sample size is 20. Granularity uses 32/256/2,048 base
iterations. No wall-clock threshold is a correctness test.

## TP-006 contention measurements

Two additional groups validate the multi-worker sparse-arrival and ticket
cancellation paths:

| Group | Timed operation | Outside per-operation timing |
| --- | --- | --- |
| `thread_pool_sparse_multiworker` | Submit one no-op task and wait for its completion signal, with 1/4/8/16 workers. | Before each sample, wait until all configured workers are idle; pool creation, idle re-establishment, and shutdown are excluded. |
| `thread_pool_ticket_cancellation` | One successful `PoolJobTicket::cancel_queued` call per latency sample, with queue lengths 1/128/4,096 and requested cancellation ratios 10%/50%/100%. | A worker is held on a controlled blocker while queued tickets are prepared. The minimum number of independent queues needed makes each ratio exact (for example, queue length 1 at 10% uses 10 queues). Queue setup, replacement ticket submission, worker release, draining, and shutdown are excluded. |
| `thread_pool_ticket_cancellation_contended` | Four producers submit 256 ticketed no-op tasks while four workers consume them; each per-task sample starts immediately before job preparation/submission and ends when the worker starts the task. | A persistent pool is created before the Criterion case. The reported batch throughput includes producer-thread creation and coordination. |

Ticket positions use a deterministic Fisher–Yates shuffle seeded with
`0x5eedcafe`. Between Criterion iterations, cancelled tickets are replaced
outside the accumulated cancellation duration, keeping each queue at its
configured length. The benchmark asserts queue, cancelled, and completed
counts before teardown. Per-operation latency samples are collected separately
from Criterion's aggregate measurements. The emitted p50/p95/p99 use nearest
rank over the collected samples, and each line reports the sample count plus a
10,000-interval `Instant` timer-overhead calibration. Sample collection covers
Criterion warm-up and measurement invocations; it is explicitly labelled as
such and is not Criterion's aggregate estimate presented as per-operation
quantiles.

Three formal rounds completed with Criterion sample size 20, 0.5 s warm-up,
and a 1 s measurement target. Each run used the default feature set
(`default = []`). Criterion's reported time and throughput below use the middle
estimate from each round; ticket cancellation time is for the whole cancellation
batch, while throughput counts cancelled tickets. Latency percentiles are
operation samples collected separately. For each percentile, the table gives
the median across rounds and the observed min–max range; sample counts are the
sum of operation samples from that case's Criterion warm-up and measurement
invocations.

### Sparse arrivals

| Workers | Criterion time R1 / R2 / R3 (µs) | Throughput R1 / R2 / R3 (K ops/s) | p50 ns median [range] | p95 ns median [range] | p99 ns median [range] | Samples R1–R3 min–max |
| ---: | --- | --- | --- | --- | --- | ---: |
| 1 | 21.116 / 14.836 / 13.080 | 47.356 / 67.404 / 76.450 | 15,055 [14,712–15,305] | 20,147 [19,692–24,380] | 28,125 [26,143–29,466] | 80,647–128,115 |
| 4 | 21.732 / 6.646 / 5.852 | 46.016 / 150.480 / 170.880 | 5,530 [5,018–7,832] | 12,388 [11,915–34,346] | 17,765 [15,550–76,045] | 49,143–193,215 |
| 8 | 9.622 / 7.404 / 8.216 | 103.930 / 135.060 / 121.710 | 7,160 [6,883–8,952] | 13,485 [12,065–14,681] | 17,021 [15,712–18,875] | 159,405–175,155 |
| 16 | 19.417 / 20.521 / 18.165 | 51.500 / 48.730 / 55.050 | 19,576 [19,025–19,747] | 26,483 [24,814–27,993] | 32,056 [29,467–35,170] | 70,357–80,017 |

### Ticket cancellation and concurrent submit/consume

Each row reports queue length and exact cancellation ratio. The batch field
shows `tickets cancelled / tickets queued across replicas`; for example,
4,096/10% uses 5 queues, 2,048 cancellations among 20,480 tickets. Criterion
time is per batch; operation percentiles are per successful cancellation.

| Queue / ratio | Criterion batch time R1 / R2 / R3 | Throughput R1 / R2 / R3 | p50 ns median [range] | p95 ns median [range] | p99 ns median [range] | Samples R1–R3 min–max |
| --- | --- | --- | --- | --- | --- | ---: |
| 1 / 10% (1/10) | 162.50 / 159.94 / 160.91 ns | 6.154 / 6.252 / 6.215 M/s | 160 [158–160] | 163 [163–218] | 212 [193–236] | 2,863,605–3,110,565 |
| 1 / 50% (1/2) | 160.52 / 180.46 / 163.03 ns | 6.230 / 5.541 / 6.134 M/s | 161 [160–161] | 164 [162–215] | 197 [194–235] | 3,101,535–4,199,881 |
| 1 / 100% (1/1) | 160.84 / 680.66 / 164.78 ns | 6.217 / 1.469 / 6.069 M/s | 163 [160–192] | 168 [163–230] | 206 [185–260] | 730,653–4,202,191 |
| 128 / 10% (64/640) | 15.184 / 22.007 / 15.276 µs | 4.215 / 2.908 / 4.190 M/s | 233 [232–329] | 284 [254–399] | 367 [362–560] | 2,231,232–2,836,032 |
| 128 / 50% (64/128) | 13.916 / 21.636 / 13.621 µs | 4.599 / 2.958 / 4.699 M/s | 214 [212–286] | 234 [228–386] | 317 [236–519] | 2,486,592–2,930,112 |
| 128 / 100% (128/128) | 22.286 / 29.087 / 21.978 µs | 5.744 / 4.401 / 5.824 M/s | 171 [170–222] | 183 [179–306] | 245 [196–439] | 2,526,848–3,064,448 |
| 4,096 / 10% (2,048/20,480) | 8.297 / 3.543 / 3.468 ms | 246.850 / 578.060 / 590.590 K/s | 1,691 [1,670–2,807] | 3,093 [1,783–3,538] | 3,600 [2,920–4,210] | 690,176–1,120,256 |
| 4,096 / 50% (2,048/4,096) | 5.248 / 2.293 / 2.278 ms | 390.270 / 893.100 / 899.080 K/s | 1,101 [1,095–2,029] | 1,285 [1,278–2,520] | 1,843 [1,418–3,023] | 559,104–1,382,400 |
| 4,096 / 100% (4,096/4,096) | 1,614.90 / 833.41 / 827.17 µs | 2.536 / 4.915 / 4.952 M/s | 201 [200–325] | 249 [246–633] | 311 [305–879] | 1,380,352–3,624,960 |
| Contended: 4 producers / 4 workers / 256 jobs | 399.84 / 545.52 / 407.98 µs | 640.250 / 469.280 / 627.480 K jobs/s | 27,198 [23,433–30,755] | 83,897 [60,396–84,210] | 112,771 [77,337–141,119] | 399,616–1,115,392 |

The 10,000-interval timer calibration reported p50 overheads of 22–50 ns and
p95 overheads of 24–241 ns across cases and rounds. These values are reported,
not subtracted; timer overhead is a meaningful fraction of the roughly 160 ns
single-ticket case. R1 Criterion warned that its 1 s target could not fit 20
samples for the 4,096-ticket 10%, 50%, and 100% cases, estimating 1.9 s, 1.7 s,
and 1.3 s respectively. It still collected all 20 samples and exited
successfully. The warning is a sampling-duration advisory, not evidence of a
pool defect. R2 also warned that the queue-length-128, 100% case could not fit
20 samples within the 1 s target, estimating 1.2 s; it still completed all 20
samples. Other Criterion runs also varied across rounds: queue length 128
and the contended case slowed in R2 and returned near R1 in R3, while the
4-worker sparse group changed substantially between R1 and R2/R3. The 4,096
ticket operation samples were lower in R2/R3 than R1, rather than showing a
stable worsening with round or cancellation ratio.

**Conclusion:** these measurements show queue-length-dependent cancellation
cost and machine-load variation, but do not confirm a stable severe contention
bottleneck. No structural optimization is justified by this evidence; the
existing queue and worker structure is retained.

#### Reproduction and environment

The two commands below were run once for each round, changing only the
Criterion baseline option:

```bash
cargo bench --bench thread_pool_bench --locked -- thread_pool_sparse_multiworker --sample-size 20 --warm-up-time 0.5 --measurement-time 1 --save-baseline tp006-round1
cargo bench --bench thread_pool_bench --locked -- thread_pool_ticket_cancellation --sample-size 20 --warm-up-time 0.5 --measurement-time 1 --save-baseline tp006-round1
cargo bench --bench thread_pool_bench --locked -- thread_pool_sparse_multiworker --sample-size 20 --warm-up-time 0.5 --measurement-time 1 --baseline tp006-round1
cargo bench --bench thread_pool_bench --locked -- thread_pool_ticket_cancellation --sample-size 20 --warm-up-time 0.5 --measurement-time 1 --baseline tp006-round1
```

The last two commands were repeated for R3. The initial R2 sparse command
accidentally combined mutually exclusive baseline options and exited 2:

```bash
cargo bench --bench thread_pool_bench --locked -- thread_pool_sparse_multiworker --sample-size 20 --warm-up-time 0.5 --measurement-time 1 --baseline tp006-round1 --save-baseline tp006-round2
```

The corrected R2 commands above and all R3 commands exited 0. Gnuplot was unavailable, so
Criterion used its plotters backend. The host was an Intel Xeon E5-2680 v4 (28
physical cores / 56 logical CPUs, two NUMA nodes), Ubuntu
6.8.0-147-generic x86_64, Rust 1.94.0. The source base was commit
`f02d5fd9f685193c9f605e52964931de67561d26`. The shared working tree was dirty
with TP-001 through TP-005 source, test, Rustdoc, README, guide and design
changes, as well as the TP-006 benchmark and these bilingual performance
reports. The measurements used synthetic benchmark tasks only. Those historical raw logs are unavailable in the current environment, and the exact dirty source snapshot cannot be reconstructed from the recorded base commit. The historical tables are retained as unverified historical observations; they are not claimed to be reproducible.


## Reproducible measurements: 2026-11-09

The two TP-006 groups were rerun for three rounds from implementation commit
`0a46d1802605d0f8812246870ba51d1c19e70082` (tree
`d15304c8cdf25d253bf70cfdd6540ffba375a1b9`), with no source changes during
measurement. Each run used sample size 20, 0.5 s warm-up, 1 s target measurement,
and the default feature set. R1 saved Criterion baseline
`rtp-20261109-round1`; R2 and R3 compared against that same baseline. These are
repeatability runs on one implementation, not a before/after comparison.

The raw outputs, exact command lines, exit results, environment, lockfile digest,
and SHA-256 values are in [the evidence manifest](https://github.com/qubit-ltd/rs-thread-pool/blob/d24a860a8fbc91f8a127f0bde633189c43c78000/doc/review-evidence-20261109/manifest.json)
and [archived logs](https://github.com/qubit-ltd/rs-thread-pool/tree/d24a860a8fbc91f8a127f0bde633189c43c78000/doc/review-evidence-20261109/). Sparse Criterion midpoints were
10.114–14.602 µs (1 worker), 5.997–6.730 µs (4), 10.097–25.427 µs (8), and
20.348–28.122 µs (16). Ticket-cancellation and contended measurements show
material run-to-run variation; consult each raw round rather than treating one
round as representative. Gnuplot was unavailable, so Criterion used plotters.
The host and toolchain are recorded in the manifest.

The old TP-006 tables above remain for historical context, but their original
logs were absent and their exact dirty source snapshot is unrecoverable. They
are not reproducibility evidence.

## Ticket cancellation race and long-queue check: 2026-11-10

The new `thread_pool_ticket_cancel_race` group uses persistent actors, a
per-batch barrier, immediate ticket handoff, and natural competition between
workers and cancellers. Its 32 natural cases cover 1/4 workers, 1/4 producers,
1/2 cancellers and 0/10/50/100% targets; 12 additional worker-gated cases
ensure cancellation wins can be measured. The 0% arm uses the same harness as
the cancellation arms. The historical `thread_pool_ticket_cancellation_contended`
group remains a submit/consume-only reference and is not treated as the matched
control. The scale group measures front, middle and back removal at depths
16,384 and 65,536, replenishing each removed ticket to keep queue depth fixed.

Three exclusive rounds used default features, sample size 20, the same
commit/tree, Rust 1.94.0, lockfile, and Intel Xeon E5-2680 v4 host. Each round
ran:

```bash
timeout 3600s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancel
```

The three complete outputs, identities, timestamps, host details and hashes
are archived under
[`doc/review-evidence-20261110/benchmarks`](review-evidence-20261110/benchmarks/);
the per-case table and conservation summaries are in
[`t2-measurement-summary.md`](review-evidence-20261110/benchmarks/t2-measurement-summary.md).
All rounds exited 0 and emitted all 44 race summaries. Counts satisfy
`accepted = run + cancel_callback`, `cancel_true = cancel_callback`, and
`cancel_attempts = cancel_true + cancel_false`; the unbounded race had zero
rejections. Natural competition is not forced to produce the target ratio.

For 1 worker / 4 producers / 1 canceller, the matched natural-race results
were:

| Round | Cancel target | Batch time (µs) | Throughput (K accepted jobs/s) | Submit p95/p99 (ns) | Submit-to-run p95/p99 (ns) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0% | 564.58 | 453.43 | 5,302 / 10,363 | 26,880 / 185,423 |
| 1 | 50% | 393.61 | 650.39 | 10,943 / 18,005 | 107,375 / 124,664 |
| 1 | 100% | 397.05 | 644.75 | 8,453 / 16,318 | 126,740 / 170,143 |
| 2 | 0% | 587.17 | 435.99 | 4,980 / 8,726 | 24,069 / 38,290 |
| 2 | 50% | 347.29 | 737.14 | 8,886 / 15,424 | 99,729 / 115,283 |
| 2 | 100% | 360.07 | 710.97 | 7,637 / 15,520 | 114,343 / 143,255 |
| 3 | 0% | 677.37 | 377.93 | 5,637 / 11,304 | 29,871 / 47,992 |
| 3 | 50% | 387.09 | 661.35 | 10,270 / 17,180 | 112,675 / 130,002 |
| 3 | 100% | 389.25 | 657.68 | 8,005 / 15,552 | 116,030 / 129,662 |

At 50% and 100%, submit-call p95/p99 increased in all three rounds versus the
same-harness 0% arm. Middle-position removal increased from 5.90–6.29 µs at
16,384 entries to 27.11–28.68 µs at 65,536; front removal stayed near
0.19–0.23 µs. The increase is a stable signal to evaluate the private queue
candidate; it does not establish a public performance guarantee or business
SLO violation. Higher cancellation also changes the amount of terminal work,
so accepted-job batch throughput is not a same-work comparison.

TP006 reports timer-read and atomic-recording overhead separately; these costs
were not subtracted. TP008's run/cancel calibration covers only the isolated
callback, timestamp, status, counter and channel-send path. It is not a
correction for shared-channel contention. Gated setup and release timings are
reported separately from natural-race results. The indexed FIFO candidate cut
middle/back scale latency by roughly 95–99%, but raised matched 0% submit-call
p95/p99 by 81%/62% and submit-to-run p95/p99 by 103%/83%; the historical
submit/consume control also exceeded the allowed 5% regression. The candidate
was removed, and the current FIFO remains. Details and raw outputs are in the
results summary.

## Interpretation and reproduction

Compare the same group, workload, worker count, compiler and machine. Record
commit, Rust version, hardware/OS and command with every result. Warm-up,
background load and task granularity can change rankings. Do not compare
drop-request numbers as if they included worker termination, or describe
steady-state as pool startup time. The current benchmark does not read the
historical `test-data` datasets.

## Historical observations: 2026-05-11

The tables below were preserved from the README: **historical observations,
not a performance promise for 0.10.0 or a performance guarantee for current
version 0.12.1**. The recorded environment was an Apple
M3 Max with 16 hardware threads and Rust 1.94.1. The historical command was
`cargo bench --bench thread_pool_bench -- thread_pool_submit_modes`.
That group is no longer registered by the current benchmark.

Values are Criterion mean wall-clock time (lower is better), with 2,000
tasks per case. Historically, `submit_tracked` used the same channel-based
completion wait as `submit`; those measurements excluded handle-wait
differences. They are not fresh measurements of the current steady-state or
end-to-end groups. The original README did not record a precise commit or
OS version, so exact reproduction of these historical numbers is not claimed.

#### `cpu_light`

| Workers | `ThreadPool.submit` | `ThreadPool.submit_tracked` | `FixedThreadPool.submit` | `FixedThreadPool.submit_tracked` | `threadpool.execute` | Rayon |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0.444 ms | 0.546 ms | 0.388 ms | 0.439 ms | 0.386 ms | 0.144 ms |
| 4 | 0.726 ms | 1.285 ms | 0.560 ms | 0.981 ms | 0.740 ms | 0.082 ms |
| 8 | 1.758 ms | 4.561 ms | 0.967 ms | 1.402 ms | 1.065 ms | 0.142 ms |

#### `cpu_medium`

| Workers | `ThreadPool.submit` | `ThreadPool.submit_tracked` | `FixedThreadPool.submit` | `FixedThreadPool.submit_tracked` | `threadpool.execute` | Rayon |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 2.031 ms | 2.133 ms | 2.029 ms | 2.115 ms | 2.037 ms | 1.439 ms |
| 4 | 1.354 ms | 1.057 ms | 1.321 ms | 1.296 ms | 1.455 ms | 0.425 ms |
| 8 | 1.902 ms | 3.868 ms | 0.959 ms | 1.280 ms | 2.022 ms | 0.391 ms |

#### `cpu_heavy`

| Workers | `ThreadPool.submit` | `ThreadPool.submit_tracked` | `FixedThreadPool.submit` | `FixedThreadPool.submit_tracked` | `threadpool.execute` | Rayon |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 14.256 ms | 14.360 ms | 14.251 ms | 14.198 ms | 14.157 ms | 11.078 ms |
| 4 | 4.384 ms | 4.588 ms | 4.715 ms | 4.533 ms | 4.594 ms | 3.311 ms |
| 8 | 3.505 ms | 3.502 ms | 3.391 ms | 3.993 ms | 4.335 ms | 2.965 ms |


## Related documents

See the [current design](design.md), [user guide](user_guide.md) and
[README](../README.md). Dated experiment reports remain in the repository as
historical snapshots; they and `test-data` are excluded from the crate package.
