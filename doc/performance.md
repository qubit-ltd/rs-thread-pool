# Qubit Thread Pool Performance

[中文版](performance.zh_CN.md) · Measurement boundaries for version 0.10.0.

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

## Interpretation and reproduction

Compare the same group, workload, worker count, compiler and machine. Record
commit, Rust version, hardware/OS and command with every result. Warm-up,
background load and task granularity can change rankings. Do not compare
drop-request numbers as if they included worker termination, or describe
steady-state as pool startup time. The current benchmark does not read the
historical `test-data` datasets.

## Historical observations: 2026-05-11

The tables below were preserved from the README: **historical observations,
not a performance promise for 0.10.0**. The recorded environment was an Apple
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
