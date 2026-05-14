/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::{
    panic::{
        AssertUnwindSafe,
        catch_unwind,
    },
    sync::Arc,
};

use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_lock::WaitTimeoutStatus;

use super::thread_pool_inner::ThreadPoolInner;
use super::thread_pool_state::ThreadPoolState;
use crate::{
    PoolJob,
    ThreadPoolHooks,
};

/// Worker loop entry point for dynamic thread pools.
pub(crate) struct ThreadPoolWorker;

impl ThreadPoolWorker {
    /// Runs a single worker loop until the pool asks it to exit.
    ///
    /// # Parameters
    ///
    /// * `inner` - Shared pool state used for queue access and counters.
    /// * `worker_index` - Stable worker index assigned by the pool.
    pub(crate) fn run(
        inner: Arc<ThreadPoolInner>,
        worker_index: usize,
        initial_job: Option<PoolJob>,
    ) {
        inner.hooks().run_before_worker_start(worker_index);
        let has_task_hooks = inner.hooks().has_task_hooks();
        if let Some(job) = initial_job {
            run_initial_job(&inner, job, has_task_hooks, worker_index);
        }
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
                    return;
                }
            }
        }
    }
}

/// Runs the first job assigned directly to a newly spawned worker.
///
/// The pool has already counted this job as running before releasing the worker
/// start gate. This function accepts the job, executes it when acceptance does
/// not panic, and then releases the running-task accounting slot.
///
/// # Parameters
///
/// * `inner` - Shared pool state whose running counter will be released.
/// * `job` - Initial job assigned to this worker.
/// * `has_task_hooks` - Whether per-task hooks are configured.
/// * `worker_index` - Stable index of the worker running the job.
fn run_initial_job(
    inner: &ThreadPoolInner,
    job: PoolJob,
    has_task_hooks: bool,
    worker_index: usize,
) {
    if job.accept() {
        if has_task_hooks {
            run_with_task_hooks(job, inner.hooks(), worker_index);
        } else {
            run_without_hooks(job);
        }
    }
    inner.finish_running_job();
}

/// Runs one claimed job without invoking task hooks.
///
/// # Parameters
///
/// * `job` - Claimed job to execute.
fn run_without_hooks(job: PoolJob) {
    let _ignored = catch_unwind(AssertUnwindSafe(|| job.run()));
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
    let _ignored = catch_unwind(AssertUnwindSafe(|| job.run()));
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
fn wait_for_job(inner: &ThreadPoolInner, worker_index: usize) -> Option<PoolJob> {
    loop {
        if let Some(job) = inner.try_take_queued_job() {
            return Some(job);
        }
        let mut state = inner.lock_state();
        match state.lifecycle {
            ExecutorServiceLifecycle::Running => {
                if inner.queued_count() == 0
                    && state.live_workers > state.maximum_pool_size
                    && state.live_workers > 0
                {
                    unregister_exiting_worker(inner, &mut state, worker_index);
                    return None;
                }
                drop(state);
                state = inner.lock_state();
                if state.lifecycle == ExecutorServiceLifecycle::Running
                    && state.worker_wait_is_timed()
                {
                    let keep_alive = state.keep_alive;
                    mark_thread_pool_worker_idle(inner, &mut state);
                    let mut timed_out = false;
                    if inner.queued_count() == 0 && !inner.has_pending_worker_wake() {
                        let (next_state, status) = state.wait_timeout(keep_alive);
                        state = next_state;
                        timed_out = status == WaitTimeoutStatus::TimedOut;
                    }
                    let should_retire = timed_out
                        && inner.queued_count() == 0
                        && !inner.has_pending_worker_wake()
                        && state.idle_worker_can_retire();
                    unmark_thread_pool_worker_idle(inner, &mut state);
                    if should_retire {
                        unregister_exiting_worker(inner, &mut state, worker_index);
                        return None;
                    }
                } else if state.lifecycle == ExecutorServiceLifecycle::Running {
                    mark_thread_pool_worker_idle(inner, &mut state);
                    if inner.queued_count() == 0 && !inner.has_pending_worker_wake() {
                        state = state.wait();
                    }
                    unmark_thread_pool_worker_idle(inner, &mut state);
                }
            }
            ExecutorServiceLifecycle::ShuttingDown => {
                if inner.queued_count() == 0 {
                    unregister_exiting_worker(inner, &mut state, worker_index);
                    return None;
                }
            }
            ExecutorServiceLifecycle::Stopping | ExecutorServiceLifecycle::Terminated => {
                unregister_exiting_worker(inner, &mut state, worker_index);
                return None;
            }
        }
    }
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
fn unregister_exiting_worker(
    inner: &ThreadPoolInner,
    state: &mut ThreadPoolState,
    _worker_index: usize,
) {
    inner.unregister_worker_locked(state);
}
