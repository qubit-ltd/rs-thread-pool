# Qubit Thread Pool

[![CircleCI](https://circleci.com/gh/qubit-ltd/rs-thread-pool.svg?style=shield)](https://circleci.com/gh/qubit-ltd/rs-thread-pool)
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
- Single-threaded `DelayedTaskScheduler` for cancellable delayed callbacks.
- Bounded or unbounded queue configuration.
- Lazy worker creation for the dynamic pool, with optional core-worker prestart.
- Keep-alive and optional core-thread timeout for dynamic-pool workers.
- Configurable worker thread name prefixes and stack sizes.
- `PoolJob` for advanced integrations that need to submit type-erased jobs.
- `ThreadPoolStats` for observing pool configuration and runtime counters.
- Shared `ExecutorService` lifecycle methods including `shutdown`, `shutdown_now`, and `await_termination`.
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

## Queueing and Rejection

A pool can use either an unbounded queue or a bounded queue. Bounded queues make
back pressure explicit: when the pool cannot accept a task, submission returns
`RejectedExecution::Saturated` instead of silently growing memory use.

A successful `submit` or `submit_callable` means only that the pool accepted the
task. The task can still fail, panic, or be cancelled later; observe the final
result through the returned `TaskHandle`.

## Shutdown Behavior

`shutdown` stops accepting new tasks and lets already accepted tasks finish.
`shutdown_now` stops accepting new tasks and cancels work that is still queued or
not yet started. Already running OS-thread tasks are not forcefully killed; they
finish according to their own code.

`await_termination` returns a future that resolves after shutdown has been
requested and all accepted work has completed or been cancelled.

`DelayedTaskScheduler` follows the same lifecycle shape for delayed callbacks:
`shutdown` rejects new callbacks and lets accepted callbacks run at their
deadlines, while `shutdown_now` cancels callbacks that have not started.

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

Benchmark inputs and historical comparison data are kept under `test-data`.

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

Copyright © 2026 Haixing Hu, Qubit Co. Ltd.

This project is licensed under the [Apache License, Version 2.0](LICENSE). See the `LICENSE` file in the repository for the full text.

## Author

**Haixing Hu** — Qubit Co. Ltd.

| | |
| --- | --- |
| **Repository** | [github.com/qubit-ltd/rs-thread-pool](https://github.com/qubit-ltd/rs-thread-pool) |
| **Documentation** | [docs.rs/qubit-thread-pool](https://docs.rs/qubit-thread-pool) |
| **Crate** | [crates.io/crates/qubit-thread-pool](https://crates.io/crates/qubit-thread-pool) |
