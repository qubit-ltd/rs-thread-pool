# CI coverage review evidence (2026-11-09)

## Result

The existing shared `verify` and `strict-doc` tasks cover the `async-wait` feature in the inspected CI/verifier source snapshot. The feature matrix does not contain a dedicated `async-wait` row, but this is not a coverage gap: `verify` runs the test suite with `--all-features`, and `strict-doc` runs the documentation suite with `--all-features`. The matrix was left unchanged. The final shared CI workflow was executed successfully in this worktree; see [the archived CI log](review-evidence-20261109/ci-check.log) and [validation record](review-evidence-20261109/validation.md).

## Pin and source identity

- Project base: `f082bcf74406e032d945e6a1a0c1ab41f9528af9`.
- `.infra/bootstrap-source.json` identifies the shared manager repository `https://github.com/qubit-ltd/rs-infra-tools.git` at `632214d25310d9ffaf3469b4c05b273a8f8c36b3`; the matching source checkout was present at `/home/starfish/.cache/qubit/rs-infra/sources/rs-infra-tools/632214d25310d9ffaf3469b4c05b273a8f8c36b3`.
- The project bootstrap's `config_sources.rs-infra-ci` digest is `sha256:2cc68b5ab050769382d7e9af468cb60b2261fb8e387d0ced9974c8d068964271`.
- The manager implementation at the pinned revision resolves nested shared tools from each tool repository's current `main` revision for a `latest` invocation. For CI it resolves the CI tool and its task tools together, then writes the resolved revisions to a cache invocation `manifest.txt`. The project does not have a committed lock fixing a nested `rs-infra-ci` source revision. Therefore the manager revision is pinned, while the nested CI tool revision is dynamically resolved.
- Cached CI source `rs-infra-ci` revision `2c965cad450e9f0e4d01cf1e5985e6d5e11e75b4` (commit dated 2026-11-07) and verifier revision `075f7e13c7b8383e276f9def6a08fb1f518f914e` were inspected to trace the behavior. Cache invocation manifests confirm these revisions have been used together. The CI source revision is inspection evidence only; it is not claimed to be a fixed nested pin for this worktree.

## Execution path

The project's `.infra/ci/ci.toml` selects `verify`, `strict-doc`, and `feature-matrix` (among other tasks). The checked-in `.infra/bin/ci-check.sh` enters `.infra/bootstrap.sh`, which invokes the pinned manager's `latest rs-infra-ci` entry point. The inspected CI source expands the configured tasks as follows:

| Task | Inspected command path | `async-wait` coverage |
| --- | --- | --- |
| `verify` | `rs-infra-verify run --suite test` and `rs-infra-verify run --suite doc` (along with lock, build, and package suites) | The verifier's `test` suite expands to `cargo test --locked --workspace --all-features`; its `doc` suite expands to `cargo doc --locked --workspace --all-features --no-deps`. Both include the declared `async-wait` feature. |
| `strict-doc` | `rs-infra-verify run --suite doc`, with `RUSTDOCFLAGS="-D warnings -D missing-docs"` | Uses the same all-features doc command and rejects warnings and missing docs. |
| `feature-matrix` | Shared CI reads `.infra/ci/cargo-matrix.json` and executes each declared row | Current rows are `default` (check/test/doc), `no-default-features` (check/test/doc), and `loom-model-compile` (check only). No row names `async-wait`; the separate all-features `verify` test/doc paths already cover it. |

The matrix command planner applies each row's feature/default-feature selection to every command and uses a separate `target/infra-feature-matrix` directory. This confirms that the absence of an `async-wait` row does not alter the all-features coverage supplied by `verify` and `strict-doc`.

## Supporting test-list command

Ran from the clean implementation worktree:

```text
cargo test --locked --features async-wait -- --list
```

The final captured run exited `0`. It printed 31 unit tests, 125 tests in `tests/lib_tests.rs`, one hook integration test, five stats integration tests, and 10 doctests. The 11 tests under `dynamic::thread_pool_async_wait_tests` were:

```text
test_async_termination_waits_for_exit_hook_and_notifies_all_waiters
test_capacity_changes_after_core_resize
test_capacity_changes_after_maximum_growth
test_invalid_resize_does_not_publish_capacity
test_thread_pool_await_termination_after_stop_cancels_queued_task
test_thread_pool_await_termination_can_be_cancelled
test_thread_pool_await_termination_waits_for_running_task_and_multiple_waiters
test_thread_pool_await_termination_waits_for_shutdown
test_thread_pool_capacity_changes_after_queued_ticket_cancellation
test_thread_pool_capacity_changes_on_shutdown
test_thread_pool_capacity_changes_when_worker_takes_queued_job
```

The feature is declared in `Cargo.toml` as `async-wait = ["dep:tokio"]`; the async-wait public methods and related implementation are conditionally compiled with `#[cfg(feature = "async-wait")]`. The list command was used only to identify names and cfg coverage; listing is not a test pass and is not used as such here. The captured output is archived at [`review-evidence-20261109/async-wait-test-list.log`](review-evidence-20261109/async-wait-test-list.log).

## Command and inspection record

- `git rev-parse HEAD` in the implementation worktree: `f082bcf74406e032d945e6a1a0c1ab41f9528af9` (exit 0).
- Static reads of `.infra/ci/ci.toml`, `.infra/ci/cargo-matrix.json`, `.infra/bootstrap-source.json`, `.infra/bootstrap.sh`, and `.infra/bin/ci-check.sh`: completed (exit 0).
- Static reads of the cached `rs-infra-tools` manager at `632214d...`, `rs-infra-ci` at `2c965cad...`, and `rs-infra-verify` at `075f7e1...`: completed (exit 0).
- Initial shell wrapper around `cargo test --locked --features async-wait -- --list`: wrapper exited `1` because zsh reserves the variable name `status` as read-only; the test-list command itself had run, but that wrapper did not report its exit code.
- Corrected wrapper around `cargo test --locked --features async-wait -- --list`, capturing `$?` in `cmd_exit`: exit `0`; it recorded the inventory above and ended with `all doctests ran`. This remains a list-only command, not a test pass.

No CI source, project matrix, or unrelated file was modified for T3.
