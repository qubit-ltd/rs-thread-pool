# Benchmark Test Data

This directory preserves datasets used by historical benchmark experiments.
The default `benches/thread_pool_bench.rs` does not currently load them.

## Layout

- `raw/`: downloaded compressed archives from upstream sources.
- `cpu/`: decompressed CPU-focused datasets.
- `io/`: file layouts generated from raw datasets for IO-heavy benchmarks.
- `graph/`: PBBS graph traversal datasets generated from PBBS source.

## Data Sources

- PBBS benchmark data: [cmuparlay/pbbsbench](https://github.com/cmuparlay/pbbsbench)
- Rayon demo data: [rayon-rs/rayon](https://github.com/rayon-rs/rayon)
- `fetch_datasets.sh` verifies SHA-256 checksums for all downloaded source files.
- `fetch_datasets.sh` clones PBBS with submodules to build graph generators.

## Prepare Datasets

```bash
./test-data/fetch_datasets.sh
```

By default, the script writes into this folder. You can override the target root:

```bash
THREAD_POOL_BENCH_DATA_DIR=/abs/path/to/test-data ./test-data/fetch_datasets.sh
```

The current benchmark uses generated CPU workloads and does not require these
datasets. Run it independently with:

```bash
cargo bench --bench thread_pool_bench -- --warm-up-time 0.2 --measurement-time 1 --sample-size 50
```
