# Qubit Thread Pool

[![Rust CI](https://github.com/qubit-ltd/rs-thread-pool/actions/workflows/ci.yml/badge.svg)](https://github.com/qubit-ltd/rs-thread-pool/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/endpoint?url=https://qubit-ltd.github.io/rs-thread-pool/coverage-badge.json)](https://qubit-ltd.github.io/rs-thread-pool/coverage/)
[![Crates.io](https://img.shields.io/crates/v/qubit-thread-pool.svg?color=blue)](https://crates.io/crates/qubit-thread-pool)
[![Rust](https://img.shields.io/badge/rust-1.94+-blue.svg?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![中文文档](https://img.shields.io/badge/文档-中文版-blue.svg)](README.zh_CN.md)

Qubit Thread Pool runs synchronous Rust work on OS threads, with bounded
admission, observable results and explicit shutdown. It helps services move
blocking work away from request threads without adding an async runtime.

## Installation

```toml
[dependencies]
qubit-thread-pool = "0.10"
qubit-executor = "0.8"
```

Rust 1.94 or later is required. Declare `qubit-executor` directly when
importing its `ExecutorService` trait; normal pool use requires neither
Tokio nor Rayon.

## Quick Start

A service processing bursts of blocking work can retain two core workers,
grow to eight under queue pressure, and reject excess submissions. This
minimal callable returns an observable result, then drains and terminates
the pool.

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io;

    use qubit_executor::service::ExecutorService;
    use qubit_thread_pool::ThreadPool;

    let pool = ThreadPool::builder()
        .core_pool_size(2)
        .maximum_pool_size(8)
        .queue_capacity(128)
        .thread_name_prefix("app-worker")
        .build()?;

    let handle = pool.submit_callable(|| Ok::<usize, io::Error>(40 + 2))?;
    assert_eq!(handle.get()?, 42);
    pool.shutdown();
    pool.wait_termination();
    Ok(())
}
```

For stable capacity, construct
`FixedThreadPool::builder().pool_size(4).queue_capacity(128).build()?`.
It prestarts its workers. `FixedThreadPool::default()` uses available
parallelism and an unbounded queue; use the builder to handle build errors
instead of panicking if worker creation fails.

## Choosing a Pool

| Pool | Use when | Worker policy |
| --- | --- | --- |
| `ThreadPool` | Blocking demand varies and bursts need spare capacity. | Lazy creation up to core size, then queueing, then growth up to maximum under bounded-queue pressure. |
| `FixedThreadPool` | The worker count should stay fixed. | Prestart the configured count; no resizing. |

Both provide thread name/stack configuration, worker/task hooks,
`ThreadPoolStats`, callable results and tracked tasks. Dynamic pools also
support prestart, keep-alive and optional core timeout. Neither pool schedules
async futures, implements CPU divide-and-conquer scheduling, or promises
strict task start/completion order.

## Queueing and Backpressure

A bounded queue makes overload visible as `SubmissionError::Saturated`;
handle it by limiting demand, retrying with an application policy, or
adjusting capacity. An unbounded queue can keep growing in memory and does
not trigger dynamic growth above core size merely because maximum size is
larger. Queue capacity is not a total outstanding-task limit: running work
is excluded, and dynamic worker growth can reserve an additional job slot.

`submit` accepts fire-and-forget work. Use `submit_callable` for a result,
or tracked submission for state and cancellation before execution.
Low-level `ThreadPool::submit_job` returns `PoolJobSubmissionError`,
including `AcceptancePanicked`; acceptance failure invokes neither run nor
cancel. See the guide for callback restrictions.

## Shutdown and Stop

`shutdown()` closes admission and returns without waiting for submissions
or accepted work. Follow it with `wait_termination()` to finish graceful
shutdown. `stop()` waits for in-flight admission and cancels jobs that have
not crossed the worker claim boundary; running work cannot be forcibly
killed. It is not a termination barrier. `join()` waits for work to drain
without closing admission. These waits include cancellation callbacks;
do not call them from a task on the same pool. Stats and `StopReport`
are monitoring snapshots, not synchronization barriers.

## Learn More

Read the [English user guide](doc/user_guide.md) or
[中文版用户手册](doc/user_guide.zh_CN.md) for callbacks, queue policies,
lifecycle handling and diagnostics. See the [current design](doc/design.md),
[performance and historical measurements](doc/performance.md),
[0.9 to 0.10 migration guide](doc/migration_0.9_to_0.10.md), and
[API documentation](https://docs.rs/qubit-thread-pool).

## Testing

```bash
# Run tests with the default feature set
cargo test

# Run tests with all declared features
cargo test --all-features

# Project CI checks
./ci-check.sh

# Check code coverage
./coverage.sh
```

## License

Copyright (c) 2025 - 2026. Haixing Hu. All rights reserved.

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for the
full license text.

## Contributing

Contributions are welcome. Please follow the Rust API guidelines, keep public
API documentation and tests current, and run `./align-ci.sh` to format code and
`./ci-check.sh` to satisfy CI requirements before submitting a pull request.

## Author

**Haixing Hu** - *Qubit Co. Ltd.*

Repository: [https://github.com/qubit-ltd/rs-thread-pool](https://github.com/qubit-ltd/rs-thread-pool)
