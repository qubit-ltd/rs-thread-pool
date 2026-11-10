# RTP-R2-001 / RTP-R2-003 three-round baseline measurement

All three rounds ran sequentially, with no concurrent CPU-heavy work, against
commit `245cc1a3999c9eb03feb5da2249920abf0ecef93` (tree
`51abaa03d4769a337fd31a4b5c4f381d38479561`). The benchmark source SHA-256 was
`d4fd5384fc1202e3ab8247aa5199fb616ae53c161d4f6be1459163102bd8d2f1`, and
`Cargo.lock` SHA-256 was
`f0ce0a6b6f3a034c09d6c4b657328ce6022f7a6fbf0b14707f556e65522754db`.
Environment: Intel Xeon E5-2680 v4 (28 physical / 56 logical CPUs), Linux
6.8.0-147-generic x86_64, Rust/Cargo 1.94.0, default features.

Each round used:

```bash
timeout 3600s cargo bench --locked --bench thread_pool_bench -- thread_pool_ticket_cancel
```

The filter includes the legacy cancellation group, six 16,384/65,536 queue
position cases, the historical submit/consume control, and 44 race cases
(32 natural arms and 12 worker-gated arms). Each round exited 0. Full command,
identity, host, start/end UTC, exit status, raw Criterion output, and SHA-256
are archived in `t2-timed-round-{1,2,3}/`; every `SHA256SUMS` check passes.
Criterion used 20 samples and its default 3 s warm-up / 5 s measurement target
for these cases. The recorded TP006 sample counts include Criterion warm-up and
measurement invocations. Percentiles use nearest rank. No timer or atomic
recording correction was subtracted.

## Matched natural race: 1 worker / 4 producers / 1 canceller

Each row's Criterion center time and throughput refer to a 256-job batch.
Accepted-job throughput counts both run and successfully cancelled jobs, so
this is not equal terminal work across cancellation ratios. TP006 latency
percentiles are operation-level samples from that same benchmark case.

| Round | Cancel target | Criterion time (µs) | Throughput (K accepted jobs/s) | Submit p50/p95/p99 (ns) | Submit-to-run p50/p95/p99 (ns) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0% | 564.58 | 453.43 | 1,454 / 5,302 / 10,363 | 7,700 / 26,880 / 185,423 |
| 1 | 50% | 393.61 | 650.39 | 1,874 / 10,943 / 18,005 | 55,794 / 107,375 / 124,664 |
| 1 | 100% | 397.05 | 644.75 | 1,880 / 8,453 / 16,318 | 79,878 / 126,740 / 170,143 |
| 2 | 0% | 587.17 | 435.99 | 1,204 / 4,980 / 8,726 | 7,661 / 24,069 / 38,290 |
| 2 | 50% | 347.29 | 737.14 | 1,328 / 8,886 / 15,424 | 55,379 / 99,729 / 115,283 |
| 2 | 100% | 360.07 | 710.97 | 1,402 / 7,637 / 15,520 | 65,941 / 114,343 / 143,255 |
| 3 | 0% | 677.37 | 377.93 | 1,552 / 5,637 / 11,304 | 8,768 / 29,871 / 47,992 |
| 3 | 50% | 387.09 | 661.35 | 1,665 / 10,270 / 17,180 | 62,729 / 112,675 / 130,002 |
| 3 | 100% | 389.25 | 657.68 | 1,753 / 8,005 / 15,552 | 75,274 / 116,030 / 129,662 |

At both 50% and 100%, submit-call p95 and p99 were higher than the matched
0% arm in all three rounds (50% p95 +78–106%, p99 +52–77%; 100% p95 +42–59%,
p99 +38–78%). The direction is stable and larger than the observed variation
in the matched 0% arms. The median batch time is lower at higher cancellation
ratios because more jobs terminate through cancellation; this is not a
same-work comparison and is not used as an optimization benefit claim.

Natural 50% and 100% arm cancellation attempt outcomes for this same matrix
were:

| Round | Target | Accepted | Run | Cancel callbacks = true | Attempts | False | Success / attempts |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 50% | 5,053,696 | 3,175,014 | 1,878,682 | 2,526,848 | 648,166 | 74.3% |
| 1 | 100% | 4,838,656 | 3,637,675 | 1,200,981 | 4,838,656 | 3,637,675 | 24.8% |
| 2 | 50% | 5,537,536 | 3,631,573 | 1,905,963 | 2,768,768 | 862,805 | 68.8% |
| 2 | 100% | 5,107,456 | 3,913,041 | 1,194,415 | 5,107,456 | 3,913,041 | 23.4% |
| 3 | 50% | 5,161,216 | 3,283,505 | 1,877,711 | 2,580,608 | 702,897 | 72.8% |
| 3 | 100% | 5,053,696 | 3,670,679 | 1,383,017 | 5,053,696 | 3,670,679 | 27.4% |

All these cases satisfy `accepted = run + cancel_callback`,
`cancel_true = cancel_callback`, and
`cancel_attempts = cancel_true + cancel_false`, with zero queue rejection.
The complete per-case counts for every matrix arm are in the TP007 lines of
the raw logs. Natural competition is intentionally not forced to match its
target ratio; only the gated cases force the selected cancellation count.

## Long-queue cancellation positions

| Queue depth | Position | Round 1 p50/p95/p99 (ns) | Round 2 p50/p95/p99 (ns) | Round 3 p50/p95/p99 (ns) |
| ---: | --- | --- | --- | --- |
| 16,384 | front | 189 / 199 / 259 | 189 / 204 / 279 | 189 / 318 / 418 |
| 16,384 | middle | 5,872 / 9,164 / 10,705 | 5,812 / 5,888 / 9,468 | 6,242 / 6,327 / 9,563 |
| 16,384 | back | 6,465 / 6,537 / 11,183 | 6,417 / 6,472 / 8,821 | 6,390 / 8,780 / 11,793 |
| 65,536 | front | 191 / 210 / 289 | 188 / 204 / 290 | 189 / 243 / 305 |
| 65,536 | middle | 27,014 / 29,761 / 36,043 | 27,293 / 33,482 / 38,734 | 27,172 / 32,444 / 37,066 |
| 65,536 | back | 26,493 / 30,880 / 40,587 | 26,439 / 32,381 / 43,951 | 26,365 / 47,667 / 58,232 |

Middle removal grows about 4.5x when queue depth grows 4x, stable across all
three rounds. Front removal remains near constant. This is consistent with the
current linear-position search and triggers the plan's internal optimization
evaluation; it does not establish a user-facing performance defect or SLO
violation.

## Instrumentation and conclusion

TP006 separately reports timer-read and atomic-recording samples for each case.
Typical timer p50/p95 was roughly 22–29/24–37 ns; typical atomic-recording
p50/p95 was roughly 49–70/53–137 ns, with higher outliers explicitly present
in the raw logs. TP008 standalone terminal-callback/channel instrumentation
calibration was about 86/104/156 ns for run and 85/90/142 ns for cancel
(p50/p95/p99); it measures the isolated helper path and is not a correction
for shared-channel contention. Gate setup and release are separately reported
and excluded from natural-race data.

The 20% engineering screen is exceeded by the stable high-cancellation
submit-call tail increase, and queue-position scaling is independently
confirmed. Therefore an O(1) removable FIFO candidate is being evaluated. The
candidate must improve cancellation-scale tails beyond noise while keeping the
matched 0% path within 5% of the baseline. A measured threshold crossing alone
does not justify retaining a structural change.
