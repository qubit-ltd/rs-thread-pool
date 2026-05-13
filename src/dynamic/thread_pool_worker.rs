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
use super::thread_pool_worker_runtime::ThreadPoolWorkerRuntime;
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
    /// * `worker_runtime` - Local runtime owned by this worker.
    pub(crate) fn run(
        inner: Arc<ThreadPoolInner>,
        worker_runtime: ThreadPoolWorkerRuntime,
        initial_job: Option<PoolJob>,
    ) {
        let worker_index = worker_runtime.worker_index();
        inner.hooks().run_before_worker_start(worker_index);
        let has_task_hooks = inner.hooks().has_task_hooks();
        if let Some(job) = initial_job {
            run_initial_job(&inner, job, has_task_hooks, worker_index);
        }
        loop {
            let job = wait_for_job(&inner, &worker_runtime);
            match job {
                Some(job) => {
                    if has_task_hooks {
                        run_with_task_hooks(job, inner.hooks(), worker_index);
                    } else {
                        run_without_hooks(job);
                    }
                    finish_running_job(&inner);
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
    finish_running_job(inner);
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
/// * `worker_queue` - Local queue owned by the worker requesting a job.
///
/// # Returns
///
/// `Some(job)` when work is available, or `None` when the worker should exit.
fn wait_for_job(
    inner: &ThreadPoolInner,
    worker_runtime: &ThreadPoolWorkerRuntime,
) -> Option<PoolJob> {
    let worker_index = worker_runtime.worker_index();
    let mut state = inner.lock_state();
    loop {
        match state.lifecycle {
            ExecutorServiceLifecycle::Running => {
                if let Some(job) = inner.try_take_queued_job_locked(&mut state, worker_runtime) {
                    return Some(job);
                }
                if state.live_workers > state.maximum_pool_size && state.live_workers > 0 {
                    unregister_exiting_worker(inner, &mut state, worker_index);
                    return None;
                }
                if state.worker_wait_is_timed() {
                    let keep_alive = state.keep_alive;
                    state.idle_workers += 1;
                    let (next_state, status) = state.wait_timeout(keep_alive);
                    state = next_state;
                    state.idle_workers = state
                        .idle_workers
                        .checked_sub(1)
                        .expect("thread pool idle worker counter underflow");
                    if status == WaitTimeoutStatus::TimedOut
                        && state.queued_tasks == 0
                        && state.idle_worker_can_retire()
                    {
                        unregister_exiting_worker(inner, &mut state, worker_index);
                        return None;
                    }
                } else {
                    state.idle_workers += 1;
                    state = state.wait();
                    state.idle_workers = state
                        .idle_workers
                        .checked_sub(1)
                        .expect("thread pool idle worker counter underflow");
                }
            }
            ExecutorServiceLifecycle::ShuttingDown => {
                if let Some(job) = inner.try_take_queued_job_locked(&mut state, worker_runtime) {
                    return Some(job);
                }
                unregister_exiting_worker(inner, &mut state, worker_index);
                return None;
            }
            ExecutorServiceLifecycle::Stopping | ExecutorServiceLifecycle::Terminated => {
                unregister_exiting_worker(inner, &mut state, worker_index);
                return None;
            }
        }
    }
}

/// Marks a worker-held job as finished.
///
/// # Parameters
///
/// * `inner` - Shared pool state whose running and completed counters are
///   updated.
fn finish_running_job(inner: &ThreadPoolInner) {
    let mut state = inner.lock_state();
    state.running_tasks = state
        .running_tasks
        .checked_sub(1)
        .expect("thread pool running task counter underflow");
    state.completed_tasks += 1;
    inner.notify_if_idle_or_terminated(&state);
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
    worker_index: usize,
) {
    // Migrate leftover local jobs back to the global queue before removing the
    // worker registration so queued work is not lost while this worker retires.
    state
        .queue
        .extend(inner.remove_worker_queue_locked(worker_index));
    state.live_workers = state
        .live_workers
        .checked_sub(1)
        .expect("thread pool live worker counter underflow");
    inner.notify_if_terminated(state);
}
