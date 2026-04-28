# ThreadPool Dataset Benchmark Notes (updated 2026-04-24)

## Goal

Use public datasets (instead of fully synthetic fixed-cost tasks) to evaluate ThreadPool vs Rayon on:

1. CPU-heavy and skewed workloads.
2. IO-heavy workloads.
3. Graph-traversal workloads with explicit degree-skew.

## Dataset Preparation

```bash
./test-data/fetch_datasets.sh
```

Data sources:

- PBBS benchmark data: <https://github.com/cmuparlay/pbbsbench>
- Rayon demo data: <https://github.com/rayon-rs/rayon>

Prepared local layout:

- `test-data/cpu/pbbs/wikisamp.xml` (CPU workload source)
- `test-data/io/small-files/` (4096 small files)
- `test-data/io/medium-files/` (64 medium chunk files)
- `test-data/graph/pbbs/randLocalGraph_J_10_50000` (较均衡图)
- `test-data/graph/pbbs/rMatGraph_J_12_50000` (长尾度分布图)

## Benchmark Groups

- `thread_pool_dataset_cpu`
- `thread_pool_dataset_io`
- `thread_pool_dataset_graph_traversal`

## Example Command

```bash
cargo bench --bench thread_pool_bench -- --warm-up-time 0.2 --measurement-time 1 --sample-size 50
```

Actual long-window runs in this round were executed per group:

```bash
cargo bench --bench thread_pool_bench thread_pool_dataset_cpu -- --warm-up-time 0.2 --measurement-time 1 --sample-size 50
cargo bench --bench thread_pool_bench thread_pool_dataset_io -- --warm-up-time 0.2 --measurement-time 1 --sample-size 50
cargo bench --bench thread_pool_bench thread_pool_dataset_graph_traversal -- --warm-up-time 0.2 --measurement-time 1 --sample-size 50
```

## Quick Smoke Command

```bash
cargo bench --bench thread_pool_bench thread_pool_dataset_graph_traversal -- --warm-up-time 0.01 --measurement-time 0.02 --sample-size 10
```

## Latest Smoke Snapshot (same machine, short windows)

- `dataset_cpu`, workers=4, tasks=2048:
  - ThreadPool: ~`1.43 Melem/s`
  - Rayon: ~`8.11 Melem/s`
- `dataset_cpu`, workers=8, tasks=2048:
  - ThreadPool: ~`0.45 Melem/s` (波动较大)
  - Rayon: ~`8.13 Melem/s`
- `dataset_io`, small-files, workers=4:
  - ThreadPool: ~`173 Kelem/s`
  - Rayon: ~`220 Kelem/s`
- `dataset_io`, medium-files, workers=8:
  - ThreadPool: ~`98.7 Kelem/s`
  - Rayon: ~`89.9 Kelem/s`
- `dataset_graph_traversal`, `pbbs_rmat_skewed`, workers=4:
  - ThreadPool: ~`1.52 Melem/s`
  - Rayon: ~`63.0 Melem/s`
- `dataset_graph_traversal`, `pbbs_rand_local`, workers=8:
  - ThreadPool: ~`436 Kelem/s`
  - Rayon: ~`30.3 Melem/s`

## Long-Window Snapshot (2026-04-24, same machine)

The table below uses the **middle throughput** value from Criterion `thrpt`.

| Case | ThreadPool | Rayon | TP/Rayon | Rayon speedup |
|---|---:|---:|---:|---:|
| cpu / workers=4 / tasks=512 | 1.2864 Melem/s | 5.1492 Melem/s | 24.98% | 4.00x |
| cpu / workers=4 / tasks=2048 | 1.4320 Melem/s | 8.1080 Melem/s | 17.66% | 5.66x |
| cpu / workers=8 / tasks=512 | 0.3914 Melem/s | 3.3889 Melem/s | 11.55% | 8.66x |
| cpu / workers=8 / tasks=2048 | 0.4172 Melem/s | 7.9935 Melem/s | 5.22% | 19.16x |
| io small-files / workers=4 | 182.70 Kelem/s | 221.98 Kelem/s | 82.30% | 1.21x |
| io small-files / workers=8 | 154.56 Kelem/s | 158.67 Kelem/s | 97.41% | 1.03x |
| io medium-files / workers=4 | 77.906 Kelem/s | 86.531 Kelem/s | 90.03% | 1.11x |
| io medium-files / workers=8 | 96.033 Kelem/s | 88.758 Kelem/s | 108.20% | 0.92x |
| graph rand-local / workers=4 | 1.4865 Melem/s | 77.123 Melem/s | 1.93% | 51.88x |
| graph rand-local / workers=8 | 0.4368 Melem/s | 30.886 Melem/s | 1.41% | 70.71x |
| graph rmat-skewed / workers=4 | 1.5111 Melem/s | 71.752 Melem/s | 2.11% | 47.48x |
| graph rmat-skewed / workers=8 | 0.4272 Melem/s | 30.260 Melem/s | 1.41% | 70.84x |

### Delta vs Previous Long-Window (2026-04-23)

ThreadPool median throughput changes (same benchmark groups, same machine):

1. CPU: `+2.4%` to `+10.5%`.
2. IO: `+1.6%` to `+20.6%`.
3. Graph: `+0.2%` to `+30.6%`.

### Key Takeaways

1. CPU and graph traversal workloads still show a large scheduling/coordination gap vs Rayon.
2. IO-heavy workloads remain much closer than CPU/graph, and `medium-files / workers=8` is now above Rayon.
3. Graph workloads still expose the biggest gap, especially under skewed frontier expansion.

## Notes

- Smoke run uses very short measurement windows and should only be treated as a sanity check.
- For decision-grade comparisons, use the full command with longer warm-up and measurement windows.
