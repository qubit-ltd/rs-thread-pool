# Qubit Thread Pool

[![Rust CI](https://github.com/qubit-ltd/rs-thread-pool/actions/workflows/ci.yml/badge.svg)](https://github.com/qubit-ltd/rs-thread-pool/actions/workflows/ci.yml)
[![Coverage Status](https://coveralls.io/repos/github/qubit-ltd/rs-thread-pool/badge.svg?branch=main)](https://coveralls.io/github/qubit-ltd/rs-thread-pool?branch=main)
[![Crates.io](https://img.shields.io/crates/v/qubit-thread-pool.svg?color=blue)](https://crates.io/crates/qubit-thread-pool)
[![Rust](https://img.shields.io/badge/rust-1.94+-blue.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![中文文档](https://img.shields.io/badge/文档-中文版-blue.svg)](README.zh_CN.md)

Thread-pool executor services for Rust.

## Overview

Qubit Thread Pool provides OS-thread based `ExecutorService` implementations for
synchronous work. It contains a dynamic `ThreadPool` for bursty workloads, a
`FixedThreadPool` for stable worker counts, and a `DelayedTaskScheduler` for
cancellable deadline-based callbacks.

The crate is built on `qubit-executor`, so it shares the same task acceptance,
shutdown, cancellation, and `TaskHandle` semantics as other Qubit executor
implementations. It does not require Tokio or Rayon for normal use.

## Features

- Dynamic `ThreadPool` with separate core and maximum worker limits.
- Fixed-size `FixedThreadPool` for predictable worker counts.
- `FixedThreadPool` implements `Default`: it is built from `FixedThreadPoolBuilder::default()` with the same defaults (worker count from available parallelism, unbounded queue, default thread name prefix). On failure to spawn workers it panics; use `FixedThreadPoolBuilder::default().build()` when you need a `Result`.
- Single-threaded `DelayedTaskScheduler` for cancellable delayed callbacks.
- Bounded or unbounded queue configuration.
- Lazy worker creation for the dynamic pool, with optional core-worker prestart.
- Keep-alive and optional core-thread timeout for dynamic-pool workers.
- Configurable worker thread name prefixes and stack sizes.
- Worker and task lifecycle hooks for lightweight instrumentation.
- `ThreadPoolStats` for observing pool configuration and runtime counters.
- Shared `ExecutorService` lifecycle methods including `shutdown`, `stop`, and `wait_termination`.
- Criterion benchmarks and test data for comparing Qubit pools with `threadpool` and Rayon.

## Pool Models

`ThreadPool` is modeled after the common core-size / maximum-size executor
pattern. It creates workers lazily up to the core size, queues tasks after that,
and grows toward the maximum size when a bounded queue cannot accept more work.
This is useful when load is uneven and you want short bursts to use additional
threads without keeping them alive forever.

`FixedThreadPool` starts and maintains a fixed number of workers. It is useful
when capacity planning is simple, when worker count should be stable, or when
predictable scheduling is more important than dynamic growth.

`FixedThreadPool::default()` is equivalent to `FixedThreadPoolBuilder::default().build()` except that build errors become a panic; prefer the builder's `build()` when you must handle `ExecutorServiceBuilderError`.

Internally, the dynamic pool keeps a global FIFO queue for externally submitted
work and uses worker-owned deques with registered stealers for direct handoff
to newly created workers. The fixed pool uses a lock-free global injector with
targeted idle-worker wakeups, which keeps the fire-and-forget submit path small
and predictable.

## Queueing and Rejection

A pool can use either an unbounded queue or a bounded queue. Bounded queues make
back pressure explicit: when the pool cannot accept a task, submission returns
`SubmissionError::Saturated` instead of silently growing memory use.

A successful `submit` means only that the pool accepted a fire-and-forget
runnable. Use `submit_callable` when you need a `TaskHandle` for the final
result, or `submit_tracked` / `submit_tracked_callable` when you also need
status and pre-start cancellation.

## Lifecycle Hooks

Both `ThreadPoolBuilder` and `FixedThreadPoolBuilder` support optional hooks for
worker and task instrumentation:

- `before_worker_start`
- `after_worker_stop`
- `before_task`
- `after_task`

Each hook receives the stable worker index and runs on the worker thread. Hook
panics are caught and ignored so instrumentation cannot terminate workers or
corrupt executor accounting. Keep hooks short; they are part of the execution
hot path.

```rust
use qubit_thread_pool::{ExecutorService, FixedThreadPool};

let pool = FixedThreadPool::builder()
    .pool_size(4)
    .before_task(|worker| {
        std::hint::black_box(worker);
    })
    .after_task(|worker| {
        std::hint::black_box(worker);
    })
    .build()?;

pool.submit(|| Ok::<(), std::io::Error>(()))?;
pool.shutdown();
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Shutdown Behavior

`shutdown` stops accepting new tasks and lets already accepted tasks finish.
`stop` stops accepting new tasks and cancels work that is still queued or
not yet started. Already running OS-thread tasks are not forcefully killed; they
finish according to their own code.

`wait_termination` blocks the current thread after shutdown has been requested
until all accepted work has completed or been cancelled.

`DelayedTaskScheduler` follows the same lifecycle shape for delayed callbacks:
`shutdown` rejects new callbacks and lets accepted callbacks run at their
deadlines, while `stop` cancels callbacks that have not started.

## Quick Start

### Dynamic thread pool

```rust
use std::io;

use qubit_thread_pool::{ExecutorService, ThreadPool};

let pool = ThreadPool::builder()
    .core_pool_size(2)
    .maximum_pool_size(8)
    .queue_capacity(128)
    .thread_name_prefix("app-worker")
    .build()?;

let handle = pool.submit_callable(|| Ok::<usize, io::Error>(40 + 2))?;
assert_eq!(handle.get()?, 42);
pool.shutdown();
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Fixed thread pool

```rust
use std::io;

use qubit_thread_pool::{ExecutorService, FixedThreadPool};

let pool = FixedThreadPool::builder()
    .pool_size(4)
    .queue_capacity(256)
    .build()?;

let handle = pool.submit_callable(|| Ok::<usize, io::Error>(6 * 7))?;
assert_eq!(handle.get()?, 42);
pool.shutdown();
# Ok::<(), Box<dyn std::error::Error>>(())
```

With default builder settings you can also write `let pool = FixedThreadPool::default();`—same as `FixedThreadPoolBuilder::default().build()` but panics if worker threads cannot be spawned.

### Delayed task scheduler

```rust
use std::sync::mpsc;
use std::time::Duration;

use qubit_thread_pool::DelayedTaskScheduler;

let scheduler = DelayedTaskScheduler::new("app-delay")?;
let (tx, rx) = mpsc::channel();

let handle = scheduler.schedule(Duration::from_millis(25), move || {
    tx.send("ready").expect("receiver should be alive");
})?;

assert_eq!(rx.recv()?, "ready");
assert!(!handle.cancel());
scheduler.shutdown();
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Choosing an Executor

Use `ThreadPool` when a service has blocking work with bursty traffic and needs
core/maximum worker tuning. Use `FixedThreadPool` when the desired worker count
is stable and should not grow dynamically. Use `DelayedTaskScheduler` when you
need many delayed callbacks without dedicating one sleeping OS thread per
callback; keep scheduled callbacks small and submit heavier work to a pool.

For CPU-bound divide-and-conquer work, prefer `qubit-rayon-executor`. For Tokio
applications, prefer `qubit-tokio-executor` for Tokio blocking tasks or async IO
futures. For application-level routing across all of these domains, use
`qubit-execution-services`.

## Benchmarks

This crate includes Criterion benchmarks migrated with the thread-pool code from
the original concurrent module split. The benchmarks compare Qubit thread-pool
implementations with common open-source alternatives such as `threadpool` and
Rayon under representative submission patterns.

Run the benchmark suite with:

```bash
cargo bench --bench thread_pool_bench
```

The submission-mode benchmark compares `ThreadPool.submit`,
`ThreadPool.submit_tracked`, `FixedThreadPool.submit`,
`FixedThreadPool.submit_tracked`, the external `threadpool` crate, and Rayon on
`cpu_light`, `cpu_medium`, and `cpu_heavy` tasks. CPU task costs are
deterministically varied with a bell-shaped distribution so worker scheduling
and stealing behavior are visible instead of every task completing at the same
time.

Benchmark inputs and historical comparison data are kept under `test-data`.

### Latest local run

The latest local run used
`cargo bench --bench thread_pool_bench -- thread_pool_submit_modes` on
2026-05-11 with Rust 1.94.1 on an Apple M3 Max with 16 hardware threads. The
tables below show Criterion mean wall-clock time for `thread_pool_submit_modes`;
lower is better. Each case submits 2,000 tasks. The `submit_tracked` cases use
the same channel-based task completion wait as `submit`, so the table compares
submission modes without mixing in handle wait costs.

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

## Testing

A minimal local run:

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

To mirror what continuous integration enforces, run the repository scripts from
the project root: `./align-ci.sh` brings local tooling and configuration in line
with CI, then `./ci-check.sh` runs the same checks the pipeline uses. For test
coverage, use `./coverage.sh` to generate or open reports.

## Contributing

Issues and pull requests are welcome.

- Open an issue for bug reports, design questions, or larger feature proposals when it helps align on direction.
- Keep pull requests scoped to one behavior change, fix, or documentation update when practical.
- Before submitting, run `./align-ci.sh` and then `./ci-check.sh` so your branch matches CI rules and passes the same checks as the pipeline.
- Add or update tests when you change runtime behavior, and update this README or public rustdoc when user-visible API behavior changes.
- If you change scheduling, queueing, or shutdown behavior, include tests that cover both `ThreadPool` and `FixedThreadPool` when the behavior applies to both.

By contributing, you agree to license your contributions under the [Apache License, Version 2.0](LICENSE), the same license as this project.

## License

Copyright (c) 2026. Haixing Hu.

This project is licensed under the [Apache License, Version 2.0](LICENSE). See the `LICENSE` file in the repository for the full text.

## Author

**Haixing Hu** — Qubit Co. Ltd.

| | |
| --- | --- |
| **Repository** | [github.com/qubit-ltd/rs-thread-pool](https://github.com/qubit-ltd/rs-thread-pool) |
| **Documentation** | [docs.rs/qubit-thread-pool](https://docs.rs/qubit-thread-pool) |
| **Crate** | [crates.io/crates/qubit-thread-pool](https://crates.io/crates/qubit-thread-pool) |
