# RTP-R2 final validation record

All target crate commands below ran from `/data/working/qubit/rust-common/rs-thread-pool-worktree` on the final source after removing the indexed queue candidate.

| Command | Result |
| --- | --- |
| `./.infra/bin/align-ci.sh` | Exit 0. Formatting, repository alignment, mechanical style, and strict Clippy checks passed. Earlier alignment attempts reported actionable style/Clippy issues; those were fixed and alignment rerun successfully. |
| `./.infra/bin/ci-check.sh` | Exit 0. Default and feature-matrix tests, docs/README, development checks, Loom suite, crate packaging, coverage, and security audit passed. Coverage: functions 95.66% (463/484), lines 97.51% (3283/3367), regions 97.40% (4914/5045). Audit scanned 117 locked dependencies with no vulnerability reported. |
| `RUSTFLAGS="--cfg loom" cargo test --features loom-model --lib loom_ --locked` | Exit 0; 10 model tests passed. |
| `timeout 180s cargo test --locked --all-features --test lib_tests ticket` | Exit 0; 13 ticket tests passed. |
| `timeout 180s cargo test --locked --lib notification_protocol_ -- --test-threads=1` | Exit 0; 4 deterministic notification interleaving tests passed. |
| `timeout 180s cargo test --locked --all-features --lib notification_protocol_ -- --test-threads=1` | Exit 0; 4 deterministic notification interleaving tests passed. |
| `cargo test --locked --all-features --test contracts_tests --manifest-path /tmp/rtp-r2-downstream-20261110/adapter-contracts/Cargo.toml` | Exit 0; 49 adapter contract tests passed against this worktree via temporary manifest path overrides. |
| `cargo test --locked --manifest-path /tmp/rtp-r2-downstream-20261110/execution-services/Cargo.toml -- --list` | Exit 0; discovered 42 tests and 7 doctests using temporary local dependency overrides. |
| `cargo test --locked --manifest-path /tmp/rtp-r2-downstream-20261110/execution-services/Cargo.toml --test execution_services_tests test_cpu_full_capacity_wait` | Exit 0; 2 CPU capacity stop/shutdown wake tests passed. |
| `cargo test --locked --manifest-path /tmp/rtp-r2-downstream-20261110/execution-services/Cargo.toml --test execution_services_tests test_waits_for_cpu_tokio_blocking_and_io_capacity` | Exit 0; 1 cross-domain capacity wait test passed. |
| `git diff --check` on the source-only diff | Exit 0. After raw logs were staged, a full staged diff check exited 2 because captured Criterion and downstream command output contains trailing blank lines. The raw logs were preserved unchanged to keep their recorded hashes; this does not affect Rust source or Markdown alignment. |

The broad `cargo fmt --all -- --check` diagnostic was attempted and exited nonzero because Cargo traversed the sibling `rs-executor` path dependency and found existing formatting differences there. It did not modify files. The repository-prescribed `align-ci.sh` subsequently passed on the target project. This is not treated as a target crate check.

Two early attempts to list execution-services tests in the temporary harness failed before validation: one resolved the registry Rayon executor rather than the local workspace version, and one omitted docs/examples copied from the package. The temporary manifest/dependency setup was corrected, the successful commands above passed, and no downstream repository files were changed. Full failure output is retained in this directory.

## Measurement-gated optimization decision

The three baseline and three candidate benchmark rounds each exited 0 and produced 44 race summaries. Hashes and raw logs are recorded in the benchmark round directories and `manifest.json`. The indexed queue improved middle/back cancellation latency by roughly 95–99%, but exceeded the plan's 5% normal-path regression limit (0% submit p95/p99 +81.3%/+62.3%, historical contended throughput -10.2%). It was removed from final source. The existing `VecDeque` queue remains; measurements and the decision are documented in the benchmark summary and bilingual performance docs.

The deterministic notification tests were also checked with a temporary notification mutation during implementation; the idle-publish case failed under the mutation and passed after restoration. No mutation remains in the final source.

## Final record correction

After the evidence logs were staged, a full `git diff --check` returned exit 2 for trailing blank lines in captured raw outputs; the source-only check had exited 0. Those logs remain byte-for-byte unchanged because their SHA-256 values are part of the evidence. This record was corrected, then `align-ci.sh` passed and `ci-check.sh` was rerun. The first post-correction CI attempt exited 1 in the feature-matrix `cargo test --workspace` step at the pre-existing `test_fixed_worker_wake_with_older_termination_waiter` assertion (`idle_worker_count`, line 1225). That test's code is unchanged from baseline. The exact test passed 10 consecutive isolated runs, and the next full `ci-check.sh` exited 0, including the feature matrix, Loom, package, coverage, and audit suites. The intermittent failure was observed once; its scheduling trigger was not established, and no speculative fix was made.
