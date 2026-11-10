// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::hint::spin_loop;
use std::sync::Arc;

use qubit_executor::service::ExecutorServiceLifecycle;

use super::fixed_thread_pool_inner::FixedThreadPoolInner;
use super::fixed_thread_pool_state::FixedThreadPoolState;
use super::fixed_worker_runtime::FixedWorkerRuntime;
use crate::PoolJob;
use crate::ThreadPoolHooks;
use crate::internal::contain_panic;

/// Number of short queue probes before a fixed worker parks.
const IDLE_SPIN_LIMIT: usize = 256;

/// Worker loop entry point for fixed-size thread pools.
pub(crate) struct FixedWorker;

impl FixedWorker {
    /// Runs one fixed-pool worker loop.
    ///
    /// # Parameters
    ///
    /// * `inner` - Shared fixed-pool state.
    /// * `worker_runtime` - Queue runtime owned by this worker.
    pub(super) fn run(inner: Arc<FixedThreadPoolInner>, worker_runtime: FixedWorkerRuntime) {
        let worker_index = worker_runtime.worker_index();
        inner.hooks().run_before_worker_start(worker_index);
        let has_task_hooks = inner.hooks().has_task_hooks();
        loop {
            if let Some(job) = inner.try_take_job() {
                if has_task_hooks {
                    run_with_task_hooks(job, inner.hooks(), worker_index);
                } else {
                    run_without_hooks(job);
                }
                inner.finish_running_job();
                continue;
            }
            if !wait_for_fixed_pool_work(&inner) {
                break;
            }
        }
        inner.hooks().run_after_worker_stop(worker_index);
        worker_exited(&inner, worker_index);
    }
}

/// Runs one claimed job without invoking task hooks.
///
/// # Parameters
///
/// * `job` - Claimed job to execute.
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
fn run_with_task_hooks(job: PoolJob, hooks: &ThreadPoolHooks, worker_index: usize) {
    hooks.run_before_task(worker_index);
    run_without_hooks(job);
    hooks.run_after_task(worker_index);
}

/// Waits until visible work exists or the worker should exit.
///
/// # Parameters
///
/// * `inner` - Shared fixed-pool state.
///
/// # Returns
///
/// `true` when the worker should try to take work again, or `false` when it
/// should exit.
fn wait_for_fixed_pool_work(inner: &FixedThreadPoolInner) -> bool {
    let mut state = inner.state.lock();
    loop {
        match state.lifecycle {
            ExecutorServiceLifecycle::Running => {
                if inner.queued_count() > 0 {
                    return true;
                }
                drop(state);
                if spin_for_fixed_pool_work(inner) {
                    return true;
                }
                state = inner.state.lock();
                if state.lifecycle != ExecutorServiceLifecycle::Running {
                    continue;
                }
                mark_fixed_worker_idle(inner, &mut state);
                if inner.queued_count() > 0 || inner.has_pending_worker_wake() {
                    unmark_fixed_worker_idle(inner, &mut state);
                    return true;
                }
                #[cfg(test)]
                inner
                    .notification_control()
                    .pause(crate::tests::notification_control::Phase::IdleWorkerWaitBoundary);
                state.wait();
                unmark_fixed_worker_idle(inner, &mut state);
            }
            ExecutorServiceLifecycle::ShuttingDown => {
                if inner.queued_count() > 0 {
                    return true;
                }
                if inner.inflight_count() == 0 {
                    return false;
                }
                mark_fixed_worker_idle(inner, &mut state);
                if inner.queued_count() == 0 && inner.inflight_count() > 0 {
                    #[cfg(test)]
                    inner.notification_control().pause(
                        crate::tests::notification_control::Phase::ShutdownWaitBoundary,
                    );
                    state.wait();
                }
                unmark_fixed_worker_idle(inner, &mut state);
            }
            ExecutorServiceLifecycle::Stopping | ExecutorServiceLifecycle::Terminated => {
                return false;
            }
        }
    }
}

/// Briefly probes for new work before a worker enters the parked idle path.
///
/// # Parameters
///
/// * `inner` - Fixed pool whose queue counters are checked.
///
/// # Returns
///
/// `true` when work or a pending wake appears during the spin window.
fn spin_for_fixed_pool_work(inner: &FixedThreadPoolInner) -> bool {
    if inner.pool_size() <= 4 {
        return false;
    }
    for _ in 0..IDLE_SPIN_LIMIT {
        if inner.queued_count() > 0 || inner.has_pending_worker_wake() {
            return true;
        }
        if !inner.is_admission_open() {
            return false;
        }
        spin_loop();
    }
    false
}

/// Marks a fixed-pool worker as idle in locked and lock-free state.
///
/// # Parameters
///
/// * `inner` - Fixed pool whose idle counter is updated.
/// * `state` - Locked mutable state containing authoritative idle workers.
fn mark_fixed_worker_idle(inner: &FixedThreadPoolInner, state: &mut FixedThreadPoolState) {
    state.idle_workers += 1;
    inner.mark_worker_idle();
}

/// Marks a fixed-pool worker as no longer idle.
///
/// # Parameters
///
/// * `inner` - Fixed pool whose idle counter is updated.
/// * `state` - Locked mutable state containing authoritative idle workers.
///
/// # Panics
///
/// Panics if the idle worker counter is already zero, which violates the
/// invariant that each idle worker is unmarked exactly once.
fn unmark_fixed_worker_idle(inner: &FixedThreadPoolInner, state: &mut FixedThreadPoolState) {
    state.idle_workers = state
        .idle_workers
        .checked_sub(1)
        .expect("fixed pool idle worker counter underflow");
    inner.unmark_worker_idle();
}

/// Marks one fixed-pool worker as exited.
///
/// # Parameters
///
/// * `inner` - Shared fixed-pool state.
/// * `_worker_index` - Index of the exiting worker.
///
/// # Panics
///
/// Panics if the live worker counter is already zero, which violates the
/// invariant that each worker exit is recorded exactly once.
fn worker_exited(inner: &FixedThreadPoolInner, _worker_index: usize) {
    inner.state.with_write_notify_all(|state| {
        state.live_workers = state
            .live_workers
            .checked_sub(1)
            .expect("fixed pool live worker counter underflow");
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;

    use super::super::fixed_thread_pool_inner::FixedThreadPoolInner;
    use super::FixedWorker;
    use super::FixedWorkerRuntime;
    use crate::PoolJob;
    use crate::ThreadPoolHooks;

    struct PanickingCancelCapture;

    impl Drop for PanickingCancelCapture {
        fn drop(&mut self) {
            panic!("unused cancel capture destructor panic");
        }
    }

    fn assert_worker_survives_cancel_capture_drop_panic(task_hooks: bool) {
        let worker_stops = Arc::new(AtomicUsize::new(0));
        let task_hook_calls = Arc::new(AtomicUsize::new(0));
        let mut hooks = ThreadPoolHooks::new().after_worker_stop({
            let worker_stops = Arc::clone(&worker_stops);
            move |_| {
                worker_stops.fetch_add(1, Ordering::Relaxed);
            }
        });
        if task_hooks {
            hooks = hooks
                .before_task({
                    let task_hook_calls = Arc::clone(&task_hook_calls);
                    move |_| {
                        task_hook_calls.fetch_add(1, Ordering::Relaxed);
                    }
                })
                .after_task({
                    let task_hook_calls = Arc::clone(&task_hook_calls);
                    move |_| {
                        task_hook_calls.fetch_add(1, Ordering::Relaxed);
                    }
                });
        }

        let inner = Arc::new(FixedThreadPoolInner::with_hooks(1, None, hooks));
        inner.reserve_worker_slot();
        let worker_inner = Arc::clone(&inner);
        let worker = thread::spawn(move || FixedWorker::run(worker_inner, FixedWorkerRuntime::new(0)));

        let first_job = PoolJob::new(
            Box::new(|| {}),
            Box::new({
                let capture = PanickingCancelCapture;
                move || drop(capture)
            }),
        );
        inner.submit(first_job).expect("first job should be accepted");

        let (second_done_tx, second_done_rx) = mpsc::channel();
        inner
            .submit(PoolJob::new(
                Box::new(move || second_done_tx.send(()).expect("test should receive second job")),
                Box::new(|| {}),
            ))
            .expect("second job should be accepted");

        assert!(
            second_done_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "worker should execute the job after the destructor panic"
        );
        let started = Instant::now();
        while inner.stats().completed_tasks != 2 && started.elapsed() < Duration::from_secs(2) {
            thread::yield_now();
        }
        let stats = inner.stats();
        assert_eq!(stats.completed_tasks, 2);
        assert_eq!(stats.running_tasks, 0);

        let (join_done_tx, join_done_rx) = mpsc::channel();
        let join_inner = Arc::clone(&inner);
        thread::spawn(move || {
            join_inner.wait_until_idle();
            let _sent = join_done_tx.send(());
        });
        assert!(
            join_done_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "join should finish after both tasks complete"
        );
        inner.shutdown();
        assert!(inner.wait_for_termination_timeout(Duration::from_secs(2)));
        worker.join().expect("fixed worker should exit normally");
        assert_eq!(worker_stops.load(Ordering::Relaxed), 1);
        assert_eq!(task_hook_calls.load(Ordering::Relaxed), usize::from(task_hooks) * 4);
    }

    #[test]
    fn test_worker_contains_cancel_capture_drop_panic_without_task_hooks() {
        assert_worker_survives_cancel_capture_drop_panic(false);
    }

    #[test]
    fn test_worker_contains_cancel_capture_drop_panic_with_task_hooks() {
        assert_worker_survives_cancel_capture_drop_panic(true);
    }
}
