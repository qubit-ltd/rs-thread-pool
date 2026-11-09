# Implementation validation record (2026-11-09)

Project: `/data/working/qubit/rust-common/rs-thread-pool`
Implementation worktree: `/data/working/qubit/.worktrees/rs-thread-pool/implement-20261109`
Implementation commit: `0a46d1802605d0f8812246870ba51d1c19e70082`
Rust: `rustc 1.94.0 (4a4ef493e 2026-03-02)`; Cargo `1.94.0`
Host: Ubuntu 6.8.0-147-generic x86_64; Intel Xeon E5-2680 v4, 56 logical CPUs.

## Targeted regressions

| Command | Result |
| --- | --- |
| `cargo test --locked --test lib_tests thread_pool_builder` (pre-change baseline) | exit 0; 14 passed. |
| `cargo test --locked --test lib_tests thread_pool_builder` (RED, after adding NUL regressions) | exit 101 as expected: dynamic lazy build accepted a NUL prefix; fixed builder panicked in standard thread naming. Unicode and empty-prefix cases passed. |
| `cargo test --locked --test lib_tests thread_pool_builder` (after T1) | exit 0; 18 passed. |
| `cargo test --locked --lib test_worker_reservation_rolls_back_during_unwind` | exit 0; 1 passed. |
| `cargo test --locked --lib` (after T1) | exit 0; 32 passed. |
| `timeout 120s cargo test --locked --lib pool_job` | exit 0; 4 passed. |
| `timeout 120s cargo test --locked --lib acceptance_guard_unwind` | exit 0; 1 passed. |
| `timeout 120s cargo test --locked --lib acceptance_slot_guard_unwind` | exit 0; 1 passed. |
| `timeout 120s cargo test --locked --test lib_tests thread_pool_failure` | exit 0; 10 passed. |
| `timeout 120s cargo test --locked --test thread_pool_hooks_tests` | exit 0; 1 passed. |
| `cargo fmt --all` | exit 0. Repository alignment later applied the project formatter successfully. |
| `git diff --check` | exit 0. |
| `cargo test --locked --features async-wait -- --list` | exit 0; inventory recorded in [async-wait-test-list.log](async-wait-test-list.log), including 11 async-wait tests. Listing only; not counted as test execution. |

During T2 iteration, an initial focused compile failed because the new fixed guard had insufficient visibility; visibility was corrected. A subsequent new stop regression exposed a race because the blocker was released before stop began; the test now waits on `is_not_running()` before release. A test setup initially attempted to clone `ThreadPool` directly and was changed to use `Arc<ThreadPool>`. These intermediate issues were corrected, and the focused commands above passed.

## Repository alignment and CI

| Command | Result |
| --- | --- |
| `./.infra/bin/align-ci.sh` (initial) | exit 1. Style reported missing seven-line license headers and parameter Rustdoc in new guard/helper modules, and a reservation type/file naming mismatch. Fixed those exact findings. Full output: [align-ci-initial.log](align-ci-initial.log). |
| `./.infra/bin/align-ci.sh` (after fixes) | exit 0; Rust style and `rs-infra-ci check` succeeded. Full output: [align-ci.log](align-ci.log). |
| `./.infra/bin/align-ci.sh` (final evidence/doc tree) | exit 0; Rust style and `rs-infra-ci check` succeeded. Full output: [align-ci-final.log](align-ci-final.log). |
| `./.infra/bin/ci-check.sh` (initial integrated code run) | exit 0; full output: [ci-check.log](ci-check.log). |
| `./.infra/bin/ci-check.sh` (final code and documentation) | exit 0; full output: [ci-check-final.log](ci-check-final.log). |

The CI run passed lock, build, verify tests, verify docs, README, default/no-default feature matrix, Clippy/style gates, strict Rustdoc, Loom (4 model tests), package verification, coverage, and dependency audit. Coverage was functions 95.59% (434/454), lines 97.69% (3049/3121), regions 97.64% (4630/4742), meeting the repository thresholds of >=95%, >90%, and >85%. The final audit scanned 117 locked third-party dependencies and succeeded without reporting a vulnerability.

RTP-004: the checked-in matrix has no standalone async-wait row. The shared `verify` test/doc and `strict-doc` paths inspected for the actual CI invocation use `--all-features`; CI passed those paths, so async-wait test and documentation coverage already exist. No duplicate matrix row was added. Details are in [CI coverage evidence](../review-evidence-20261109-ci.md).

The initial align failure is retained here as a diagnostic record; it was fixed and the required alignment and CI gates then passed.

## Compatibility and performance evidence

| Command | Result |
| --- | --- |
| `cargo test --locked --manifest-path /tmp/rs-executor-adapter-contracts-rpt-20261109/Cargo.toml` (temporary manifest points at this worktree's `rs-thread-pool`) | exit 0; 48 adapter contract tests passed. The temporary copy was created because the checked-in manifest resolves the normal sibling checkout. |
| Three-round sparse and ticket-cancellation Criterion commands listed in [`manifest.json`](manifest.json) | All six benchmark invocations completed successfully; each raw output is archived in this directory. |

The older TP-006 figures already present in the performance reports are retained as historical data. Their referenced `/tmp/rs-thread-pool-tp006-{sparse,cancel}-round{1,2,3}.log` files were not present, and the exact dirty source snapshot cannot be reconstructed from the recorded base commit. They are therefore not presented as reproducible results. The newly archived runs below were measured from implementation commit `0a46d1802605d0f8812246870ba51d1c19e70082` with a clean source tree; the evidence manifest records its tree, lockfile digest, host, commands, and log hashes.

The three runs are repeatability observations of the same code, not before/after measurements. Several Criterion comparisons varied materially across rounds, especially sparse 8/16-worker cases; no stable performance conclusion is drawn. Criterion used its plotters backend because Gnuplot was unavailable.
