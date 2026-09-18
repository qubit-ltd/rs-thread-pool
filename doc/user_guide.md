# Qubit Thread Pool User Guide

[中文版](user_guide.zh_CN.md) · Applies to `qubit-thread-pool` 0.10.0 and Rust 1.94 or later.

## Purpose and Audience

This guide is for Rust services that must run blocking or otherwise synchronous
work away from the submitting thread. It explains how to select and operate a
Qubit pool when accepting too much work, waiting for results, or shutting down
carelessly would be a production concern. It does not turn CPU-bound
divide-and-conquer work into a replacement for Rayon, and it does not schedule
async futures.

## Conceptual Model

`ThreadPool` is elastic: it creates workers lazily through its core size,
queues work, and can grow up to its maximum only when a bounded queue is full.
`FixedThreadPool` starts a fixed number of workers and never resizes. Both
implement `ExecutorService`; accepting a task and its eventual task result are
different outcomes.

| Term | Meaning |
| --- | --- |
| Core size | The dynamic pool's ordinary worker target. |
| Maximum size | The dynamic pool's upper worker limit during bounded-queue pressure. |
| Queue capacity | Number of waiting tasks, excluding tasks already held by workers. |
| `TaskHandle` | A handle returned by callable submission; `get()` observes task completion. |

## Scenario: a Bursty Blocking Conversion Service

Assume an HTTP-facing service converts uploaded documents with blocking code.
It normally needs two workers, may use eight for a burst, and must refuse excess
work rather than accumulate it indefinitely. Success means an accepted
conversion yields its output, while overload is visible to the caller.

## Installation and Minimal Configuration

Add the crate with the version selected for your project:

```toml
[dependencies]
qubit-thread-pool = "0.10"
```

The examples import `ExecutorService` because its submission and lifecycle
methods are trait methods.

## Core Workflow

Build the elastic pool with a bounded queue, submit a callable, and wait for its
result. The result below is observable as `42`.

```rust
use std::io;

use qubit_executor::service::ExecutorService;
use qubit_thread_pool::ThreadPool;

let pool = ThreadPool::builder()
    .core_pool_size(2)
    .maximum_pool_size(8)
    .queue_capacity(128)
    .thread_name_prefix("convert-worker")
    .build()?;

let handle = pool.submit_callable(|| Ok::<usize, io::Error>(40 + 2))?;
assert_eq!(handle.get()?, 42);

pool.shutdown();
pool.wait_termination();
# Ok::<(), Box<dyn std::error::Error>>(())
```

Use `submit` for fire-and-forget runnables. Use `submit_callable` when a value
or task error must be observed. `join()` waits for accepted work to drain but
does not request shutdown, so later submissions are still possible.

## Advanced Usage

Choose `FixedThreadPool` when capacity is stable; it prestarts its configured
workers. Configure `prestart_core_threads()` on the builder, or call
`prestart_all_core_threads()` on an existing dynamic pool, when startup latency is
more important than lazy worker creation. Both builders accept worker and task
hooks. Hooks run on worker threads, receive a stable worker index, and ignore
their own panics; keep them short because task hooks are on the execution path.

An unbounded queue keeps accepting tasks after the dynamic pool reaches its core
size, so a larger maximum alone does not create burst workers. Choose an
unbounded queue only when that memory-growth tradeoff is acceptable.

## Errors and Diagnostics

Low-level custom jobs return `PoolJobSubmissionError`; an
`AcceptancePanicked` error means the acceptance callback panicked before the
job was published. Standard executor-service methods return the shared
`SubmissionError` type.

Builder validation reports `ExecutorServiceBuilderError`; for example, zero
queue capacity and a core size larger than the maximum are invalid. Submission
can return `SubmissionError::Saturated` for a full bounded queue or
`SubmissionError::Shutdown` once admission closes. An accepted callable can
still report its own task error through `TaskHandle::get()`.

Inspect `queued_count()`, `running_count()`, `live_worker_count()`, or `stats()`
when diagnosing pressure. These are observations, not a promise of strict task
ordering; neither pool guarantees strict start or completion order.

## Troubleshooting

- **The dynamic pool never grows above its core size.** Check whether the queue
  is unbounded. Configure `queue_capacity(...)` to enable growth under pressure.
- **A submission is rejected.** Distinguish `Saturated` from `Shutdown`; reduce
  demand or increase bounded capacity for the former, and create or retain a
  running pool for the latter.
- **A process does not finish its pool work.** Call `shutdown()` and then
  `wait_termination()` during orderly service shutdown.

## Limitations and Best Practices

`stop()` cancels queued work that has not started but cannot forcibly kill work
already executing on an OS thread. Prefer `shutdown()` for graceful draining.
Bound queue capacity according to the memory footprint and latency budget of
queued tasks. Select pool sizes at construction time for normal operation;
runtime size setters are intended for explicit control-plane adjustments.

## Further Reading

Read the [README](../README.md), the [中文版用户手册](user_guide.zh_CN.md), and
the [API documentation](https://docs.rs/qubit-thread-pool).
