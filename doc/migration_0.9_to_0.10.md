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
- Dynamic workers are created before a directly assigned job is accepted, and
  acceptance runs outside the pool monitor. A worker-spawn rejection therefore
  does not invoke acceptance, run, or cancellation callbacks.

The ordinary `ExecutorService` submission methods keep their existing
`SubmissionError` contract. Review calls to `join()` and
`wait_termination()` when they run from worker tasks, because a task must not
wait for its own pool to become idle.
