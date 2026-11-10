# Notification protocol test evidence

All commands ran from the `rs-thread-pool` crate directory. The timeout wrapper
was `timeout 120s`; tests used one test thread. Complete command output is in the
linked logs. Exit status is recorded here because the shell printed it after
the `tee` pipeline.

| Run | Command | Exit | Result |
| --- | --- | ---: | --- |
| Initial compile attempt | `cargo test --locked --lib notification_protocol_ -- --test-threads=1` | 101 | No tests ran. Compilation identified a missing cfg(test) `FixedThreadPool::submit_job` helper; added the wrapper and reran. [Output](initial-compile-failure.log) |
| Ordinary filtered run | `cargo test --locked --lib notification_protocol_ -- --test-threads=1` | 0 | 4 passed. [Output](ordinary-pass.log) |
| All-features filtered run | `cargo test --locked --all-features --lib notification_protocol_ -- --test-threads=1` | 0 | 4 passed. [Output](all-features-pass.log) |
| Negative sensitivity mutation | `cargo test --locked --lib notification_protocol_idle_publish -- --test-threads=1` | 101 | Temporarily replaced the dynamic pool's `wake_one_idle_worker` notification (`self.lock_state().notify_all()`) with lock acquisition only. `notification_protocol_idle_publish` failed after 5.01 seconds because its sole published task did not run. [Output](negative-idle-notification.log) |
| Restored ordinary filtered run | `cargo test --locked --lib notification_protocol_ -- --test-threads=1` | 0 | Restored source; 4 passed. [Output](restored-ordinary-pass.log) |
| Restored all-features filtered run | `cargo test --locked --all-features --lib notification_protocol_ -- --test-threads=1` | 0 | Restored source; 4 passed. [Output](restored-all-features-pass.log) |

The negative mutation was restored before the final two runs. The source diff
contains the original `self.lock_state().notify_all()` call and no mutation.
These tests exercise selected production-path interleavings; they are not an
exhaustive scheduler proof or a Loom model.
