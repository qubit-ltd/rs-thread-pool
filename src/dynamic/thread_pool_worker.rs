// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_lock::WaitTimeoutStatus;

use super::thread_pool_inner::ThreadPoolInner;
use super::thread_pool_state::ThreadPoolState;
use crate::PoolJob;
use crate::ThreadPoolHooks;
use crate::internal::contain_panic;

/// Worker loop entry point for dynamic thread pools.
pub(crate) struct ThreadPoolWorker;

impl ThreadPoolWorker {
    /// Runs a single worker loop until the pool asks it to exit.
    ///
    /// # Parameters
    ///
    /// * `inner` - Shared pool state used for queue access and counters.
    /// * `worker_index` - Stable worker index assigned by the pool.
    ///
    /// Worker lifecycle hooks surround the loop, and each claimed job is
    /// accounted for after execution even when the job panics.
    pub(crate) fn run(inner: Arc<ThreadPoolInner>, worker_index: usize) {
        inner.hooks().run_before_worker_start(worker_index);
        let has_task_hooks = inner.hooks().has_task_hooks();
        loop {
            let job = wait_for_job(&inner, worker_index);
            match job {
                Some(job) => {
                    if has_task_hooks {
                        run_with_task_hooks(job, inner.hooks(), worker_index);
                    } else {
                        run_without_hooks(job);
                    }
                    inner.finish_running_job();
                }
                None => {
                    inner.hooks().run_after_worker_stop(worker_index);
                    inner.finish_worker_exit();
                    return;
                }
            }
        }
    }
}

/// Runs one claimed job without invoking task hooks.
///
/// # Parameters
///
/// * `job` - Claimed job to execute.
///
/// A panic from the job or its captured-value destructors is contained and
/// discarded so the worker can continue.
fn run_without_hooks(job: PoolJob) {
    let _ignored = contain_panic(|| job.run());
}

/// Runs one claimed job with configured task hooks.
///
/// # Parameters
///
/// * `job` - Claimed job to execute.
/// * `hooks` - Hook set configured for the pool.
/// * `worker_index` - Stable index of the worker running the job.
///
/// The before and after hooks surround the job. A panic from the job or its
/// captured-value destructors is contained and discarded so the worker can
/// continue.
fn run_with_task_hooks(job: PoolJob, hooks: &ThreadPoolHooks, worker_index: usize) {
    hooks.run_before_task(worker_index);
    run_without_hooks(job);
    hooks.run_after_task(worker_index);
}

/// Waits until a worker can take a job or should exit.
///
/// # Parameters
///
/// * `inner` - Shared pool state and monitor wait queue.
/// * `worker_index` - Stable worker index assigned by the pool.
///
/// # Returns
///
/// `Some(job)` when work is available, or `None` when the worker should exit.
///
/// A failed timed wait falls back to one unbounded notification wait. This
/// avoids a panic or busy loop; worker retirement may be delayed until the
/// next notification.
fn wait_for_job(inner: &ThreadPoolInner, worker_index: usize) -> Option<PoolJob> {
    loop {
        if inner.should_check_maximum_retirement() {
            let mut state = inner.lock_state();
            if state.lifecycle == ExecutorServiceLifecycle::Running
                && state.live_workers > state.maximum_pool_size
            {
                unregister_exiting_worker(inner, &mut state, worker_index);
                return None;
            }
            inner.refresh_maximum_retirement_hint_locked(&state);
        }
        if let Some(job) = inner.try_take_queued_job() {
            return Some(job);
        }
        let mut state = inner.lock_state();
        match state.lifecycle {
            ExecutorServiceLifecycle::Running => {
                if inner.queued_count() == 0 && state.live_workers > state.maximum_pool_size && state.live_workers > 0 {
                    unregister_exiting_worker(inner, &mut state, worker_index);
                    return None;
                }
                if state.worker_wait_is_timed() {
                    let keep_alive = state.keep_alive;
                    mark_thread_pool_worker_idle(inner, &mut state);
                    let mut timed_out = false;
                    if inner.queued_count() == 0 && !inner.has_pending_worker_wake() {
                        let status = wait_for_keep_alive(&mut state, keep_alive);
                        timed_out = status == WaitTimeoutStatus::TimedOut;
                    }
                    let should_retire = timed_out
                        && inner.queued_count() == 0
                        && !inner.has_pending_worker_wake()
                        // An admitted submitter may still publish its job.
                        && (state.live_workers > 1 || inner.inflight_count() == 0)
                        && state.idle_worker_can_retire();
                    unmark_thread_pool_worker_idle(inner, &mut state);
                    if should_retire {
                        unregister_exiting_worker(inner, &mut state, worker_index);
                        return None;
                    }
                } else {
                    mark_thread_pool_worker_idle(inner, &mut state);
                    if inner.queued_count() == 0 && !inner.has_pending_worker_wake() {
                        #[cfg(test)]
                        inner
                            .notification_control()
                            .pause(crate::tests::notification_control::Phase::IdleWorkerWaitBoundary);
                        state.wait();
                    }
                    unmark_thread_pool_worker_idle(inner, &mut state);
                }
            }
            ExecutorServiceLifecycle::ShuttingDown => {
                if inner.queued_count() == 0 {
                    if inner.inflight_count() == 0 {
                        unregister_exiting_worker(inner, &mut state, worker_index);
                        return None;
                    }
                    mark_thread_pool_worker_idle(inner, &mut state);
                    if inner.queued_count() == 0 && inner.inflight_count() > 0 {
                        #[cfg(test)]
                        inner
                            .notification_control()
                            .pause(crate::tests::notification_control::Phase::ShutdownWaitBoundary);
                        state.wait();
                    }
                    unmark_thread_pool_worker_idle(inner, &mut state);
                }
            }
            ExecutorServiceLifecycle::Stopping | ExecutorServiceLifecycle::Terminated => {
                unregister_exiting_worker(inner, &mut state, worker_index);
                return None;
            }
        }
    }
}

/// Waits for the full keep-alive duration using bounded timer registrations.
/// Individual slice timeouts do not retire the worker before the total duration
/// has elapsed. A timer registration/completion error falls back to one
/// unbounded wait so lifecycle notifications can still wake this worker.
///
/// # Parameters
///
/// * `state` - Monitor guard protecting pool state and worker notifications.
/// * `keep_alive` - Total idle duration before the worker may retire.
///
/// # Returns
///
/// `TimedOut` when the complete duration elapses, or `Woken` after a
/// notification or timer error fallback.
fn wait_for_keep_alive(
    state: &mut qubit_lock::ParkingLotMonitorGuard<'_, ThreadPoolState>,
    keep_alive: Duration,
) -> WaitTimeoutStatus {
    wait_for_keep_alive_with(keep_alive, Duration::from_secs(3600), |timeout| match timeout {
        Some(timeout) => state.wait_for(timeout).map_err(|_| ()),
        None => {
            state.wait();
            Ok(WaitTimeoutStatus::Woken)
        }
    })
}

/// Waits in bounded slices through an injected wait operation.
///
/// # Type Parameters
///
/// * `F` - Wait operation used for timed slices and the error fallback.
///
/// # Parameters
///
/// * `keep_alive` - Total idle duration.
/// * `maximum_slice` - Maximum duration of each timer registration.
/// * `wait` - Operation that waits for a slice or performs the fallback wait.
///
/// # Returns
///
/// `TimedOut` after the full duration, or `Woken` after a notification or
/// timer error fallback.
fn wait_for_keep_alive_with<F>(keep_alive: Duration, maximum_slice: Duration, mut wait: F) -> WaitTimeoutStatus
where
    F: FnMut(Option<Duration>) -> Result<WaitTimeoutStatus, ()>,
{
    let started = Instant::now();
    loop {
        let remaining = remaining_keep_alive(keep_alive, started.elapsed());
        if remaining.is_zero() {
            return WaitTimeoutStatus::TimedOut;
        }
        match wait(Some(remaining.min(maximum_slice))) {
            Ok(WaitTimeoutStatus::Woken) => return WaitTimeoutStatus::Woken,
            Ok(WaitTimeoutStatus::TimedOut) => {}
            Err(_) => {
                let _ = wait(None);
                return WaitTimeoutStatus::Woken;
            }
        }
    }
}

/// Computes the remaining idle duration without overflowing.
///
/// # Parameters
///
/// * `keep_alive` - Configured total idle duration.
/// * `elapsed` - Time elapsed since the worker became idle.
///
/// # Returns
///
/// The remaining duration, or zero when `elapsed` reached or exceeded the
/// configured duration.
fn remaining_keep_alive(keep_alive: Duration, elapsed: Duration) -> Duration {
    keep_alive.saturating_sub(elapsed)
}

/// Marks a dynamic-pool worker as idle in locked and lock-free state.
///
/// # Parameters
///
/// * `inner` - Pool whose idle counter is updated.
/// * `state` - Locked mutable state containing authoritative idle workers.
fn mark_thread_pool_worker_idle(inner: &ThreadPoolInner, state: &mut ThreadPoolState) {
    state.idle_workers += 1;
    inner.mark_worker_idle();
}

/// Marks a dynamic-pool worker as no longer idle.
///
/// # Parameters
///
/// * `inner` - Pool whose idle counter is updated.
/// * `state` - Locked mutable state containing authoritative idle workers.
///
/// # Panics
///
/// Panics if the authoritative idle worker count is already zero.
fn unmark_thread_pool_worker_idle(inner: &ThreadPoolInner, state: &mut ThreadPoolState) {
    state.idle_workers = state
        .idle_workers
        .checked_sub(1)
        .expect("thread pool idle worker counter underflow");
    inner.unmark_worker_idle();
}

/// Marks a worker as exited.
///
/// # Parameters
///
/// * `inner` - Shared pool coordination state used for termination
///   notification.
/// * `state` - Locked mutable state whose live worker count is decremented.
/// * `worker_index` - Stable index of the exiting worker.
fn unregister_exiting_worker(inner: &ThreadPoolInner, state: &mut ThreadPoolState, _worker_index: usize) {
    state.exiting_workers = state
        .exiting_workers
        .checked_add(1)
        .expect("thread pool exiting worker counter overflow");
    inner.unregister_worker_locked(state);
    inner.refresh_maximum_retirement_hint_locked(state);
}

#[cfg(test)]
mod keep_alive_tests {
    use std::cell::Cell;
    use std::time::Duration;

    use qubit_lock::WaitTimeoutStatus;

    use super::remaining_keep_alive;
    use super::wait_for_keep_alive_with;

    #[test]
    fn test_remaining_duration_saturates_at_zero_and_handles_maximum() {
        assert_eq!(
            Duration::from_secs(3),
            remaining_keep_alive(Duration::from_secs(5), Duration::from_secs(2))
        );
        assert_eq!(
            Duration::ZERO,
            remaining_keep_alive(Duration::from_secs(5), Duration::from_secs(5))
        );
        assert_eq!(
            Duration::ZERO,
            remaining_keep_alive(Duration::from_secs(5), Duration::from_secs(6))
        );
        assert_eq!(Duration::MAX, remaining_keep_alive(Duration::MAX, Duration::ZERO));
    }

    #[test]
    fn test_large_keep_alive_slices_are_bounded_and_retried() {
        let slices = Cell::new(0);
        let status = wait_for_keep_alive_with(Duration::MAX, Duration::from_millis(1), |timeout| {
            let Some(timeout) = timeout else {
                panic!("no error expected")
            };
            assert!(timeout <= Duration::from_millis(1));
            let count = slices.get() + 1;
            slices.set(count);
            Ok(if count == 3 {
                WaitTimeoutStatus::Woken
            } else {
                WaitTimeoutStatus::TimedOut
            })
        });
        assert_eq!(WaitTimeoutStatus::Woken, status);
        assert_eq!(3, slices.get());
    }

    #[test]
    fn test_wait_error_falls_back_once_to_unbounded_wait() {
        let calls = Cell::new(0);
        let status = wait_for_keep_alive_with(Duration::MAX, Duration::from_secs(3600), |timeout| {
            calls.set(calls.get() + 1);
            if timeout.is_some() {
                Err(())
            } else {
                Ok(WaitTimeoutStatus::Woken)
            }
        });
        assert_eq!(WaitTimeoutStatus::Woken, status);
        assert_eq!(2, calls.get());
    }
}
