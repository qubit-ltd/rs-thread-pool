/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::sync::Arc;

use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_lock::WaitTimeoutStatus;

use super::thread_pool_inner::ThreadPoolInner;
use super::thread_pool_state::ThreadPoolState;
use super::thread_pool_worker_queue::ThreadPoolWorkerQueue;
use crate::PoolJob;

/// Worker loop entry point for dynamic thread pools.
pub(crate) struct ThreadPoolWorker;

impl ThreadPoolWorker {
    /// Runs a single worker loop until the pool asks it to exit.
    ///
    /// # Parameters
    ///
    /// * `inner` - Shared pool state used for queue access and counters.
    /// * `worker_queue` - Local queue owned by this worker.
    /// * `first_task` - Optional job assigned directly when the worker is spawned.
    pub(crate) fn run(
        inner: Arc<ThreadPoolInner>,
        worker_queue: Arc<ThreadPoolWorkerQueue>,
        first_task: Option<PoolJob>,
    ) {
        if let Some(job) = first_task {
            job.run();
            finish_running_job(&inner);
        }
        loop {
            let job = wait_for_job(&inner, &worker_queue);
            match job {
                Some(job) => {
                    job.run();
                    finish_running_job(&inner);
                }
                None => return,
            }
        }
    }
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
fn wait_for_job(inner: &ThreadPoolInner, worker_queue: &ThreadPoolWorkerQueue) -> Option<PoolJob> {
    let worker_index = worker_queue.worker_index();
    let mut state = inner.lock_state();
    loop {
        match state.lifecycle {
            ExecutorServiceLifecycle::Running => {
                if let Some(job) = inner.try_take_queued_job_locked(&mut state, worker_queue) {
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
                if let Some(job) = inner.try_take_queued_job_locked(&mut state, worker_queue) {
                    return Some(job);
                }
                unregister_exiting_worker(inner, &mut state, worker_index);
                return None;
            }
            ExecutorServiceLifecycle::Stopping => {
                unregister_exiting_worker(inner, &mut state, worker_index);
                return None;
            }
            ExecutorServiceLifecycle::Terminated => {
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
    inner.notify_if_terminated(&state);
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
    let requeued_jobs = inner.remove_worker_queue_locked(worker_index);
    for job in requeued_jobs {
        state.queue.push_back(job);
    }
    state.live_workers = state
        .live_workers
        .checked_sub(1)
        .expect("thread pool live worker counter underflow");
    inner.notify_if_terminated(state);
}
