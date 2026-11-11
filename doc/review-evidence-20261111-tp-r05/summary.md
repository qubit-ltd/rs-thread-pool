# TP-R05 fixed worker wake test investigation

## Result

Updated only the inline fixed worker wake test in `src/fixed/fixed_thread_pool_inner.rs`. The spare-worker and final-idle checks now poll a single `(locked idle_workers, idle_worker_count, pending_worker_wakes)` snapshot captured while holding the same monitor. Each assertion checks the captured tuple: `(1, 1, 0)` while one task runs and `(2, 2, 0)` after it is released. The existing two-second deadlines, task execution count, no-duplicate assertion, older termination waiter assertion, monitor handshakes, and cleanup assertions remain in place. No production notification protocol or public visibility changed.

The historical CI trigger remains unconfirmed. The change addresses a statically demonstrated observation-window flaw in the test: worker idle bookkeeping updates the locked count and atomic count in sequence while holding the monitor, but the former test waited on/read atomics independently and then sampled locked state separately. During a worker's idle transition those observations could span different phases. Taking the tuple under the monitor closes that observation window. This is evidence for improving test observation; it does not establish that the historical CI failure had this exact trigger or that the production protocol lost a wake.

## Environment

- Worktree commit before changes: `c1a0780e8f01b92340b44ec40b216c6f866db526`.
- Linux x86_64, kernel `6.8.0-147-generic`; 56 reported CPUs.
- Toolchain: Rust/Cargo `1.94.0`; toolchain pinned by `rust-toolchain.toml`.
- `RUST_TEST_THREADS` unset. Each focused command selects exactly one test; repetitions ran sequentially.
- Detailed environment and initial command records: `environment.txt`.

## Commands and outcomes

Each initial focused command discovered one test and passed:

- `cargo test --locked --lib test_fixed_worker_wake_with_older_termination_waiter -- --nocapture` — exit 0.
- `cargo test --locked --all-features --lib test_fixed_worker_wake_with_older_termination_waiter -- --nocapture` — exit 0.

Before the test-only change, the first command was run 50 times and the all-features command was run 50 times, sequentially. All 100 runs exited 0. After the change, each command was run another 50 times sequentially; all 100 runs exited 0. Every per-run command, exit status, and captured output is retained under `logs/`; the index is `repetitions.txt`. The captured Cargo outputs have only their trailing blank-line separators normalized so the evidence diff passes whitespace checks. No repetition failure was suppressed or followed by a retry.

The historical evidence remains `../review-evidence-20261110/validation/final-validation.md`: it records one failure during full CI, ten isolated passes, then a passing full CI, with no established scheduling trigger. The current bounded focused runs do not recreate that failure. Full parallel test-suite and CI validation are assigned to the integration task and were not run here.

## Files changed

- `src/fixed/fixed_thread_pool_inner.rs` — test observation only.
- `doc/review-evidence-20261111-tp-r05/summary.md`, `environment.txt`, `repetitions.txt`, and `logs/` — investigation evidence.
