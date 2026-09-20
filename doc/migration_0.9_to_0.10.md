# Migrating from 0.9 to 0.10

## Toolchain and dependencies

Version 0.10 requires Rust 1.94 and the Qubit dependency line
`qubit-executor 0.8`, `qubit-function 0.18`, and `qubit-lock 0.14`.
Declare `qubit-executor` directly when importing its traits or types:

```toml
qubit-thread-pool = "0.10"
qubit-executor = "0.8"
```

## API changes

- Use `qubit_executor::{...}` paths for executor types; removed convenience
  re-exports are no longer part of this crate's public API.
- `ThreadPool::submit_job` reports `PoolJobSubmissionError`. Match
  `Rejected(SubmissionError)` for admission failures and
  `AcceptancePanicked` for a panicking acceptance callback.
- Any required dynamic worker creation succeeds before acceptance runs on the
  submitting thread outside the pool monitor. Worker growth reserves an
  additional slot independently of ordinary queue capacity; all accepted jobs
  are still published to the same global `Injector`, without being bound to
  the newly created worker. A worker-spawn rejection therefore invokes none of
  the acceptance, run, or cancellation callbacks.
- `stop()` now defines cancellation at the worker claim boundary. A job may
  finish acceptance successfully while a concurrent stop is waiting, then be
  published to the global queue and cancelled before its run callback. Treat
  `StopReport` as a snapshot, and do not use `submit_job` returning `Ok(())` as
  proof that the run callback started.
- Acceptance callbacks run synchronously during submission. They may call
  `shutdown` on the same pool because it closes admission and returns without
  waiting. They must not synchronously call `stop`, `join`, or
  `wait_termination`, because those waits can deadlock behind the in-flight
  submission.

The ordinary `ExecutorService` submission methods keep their existing
`SubmissionError` contract. Review calls to `join()` and
`wait_termination()` when they run from worker tasks, because a task must not
wait for its own pool to become idle.
