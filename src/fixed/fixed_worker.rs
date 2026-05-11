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
    hint::spin_loop,
    sync::{
        Arc,
        atomic::Ordering,
    },
};

use qubit_executor::service::ExecutorServiceLifecycle;

use super::fixed_thread_pool_inner::FixedThreadPoolInner;
use super::fixed_thread_pool_state::FixedThreadPoolState;
use super::fixed_worker_runtime::FixedWorkerRuntime;
use crate::{
    PoolJob,
    ThreadPoolHooks,
};

/// Number of short queue probes before a fixed worker parks.
const IDLE_SPIN_LIMIT: usize = 256;

/// Worker loop entry point for fixed-size thread pools.
pub struct FixedWorker;

impl FixedWorker {
    /// Runs one fixed-pool worker loop.
    ///
    /// # Parameters
    ///
    /// * `inner` - Shared fixed-pool state.
    /// * `worker_runtime` - Queue runtime owned by this worker.
    pub fn run(inner: Arc<FixedThreadPoolInner>, worker_runtime: FixedWorkerRuntime) {
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
    job.run();
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
    job.run();
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
pub fn wait_for_fixed_pool_work(inner: &FixedThreadPoolInner) -> bool {
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
                state = state.wait();
                unmark_fixed_worker_idle(inner, &mut state);
            }
            ExecutorServiceLifecycle::ShuttingDown => {
                if inner.queued_count() > 0 {
                    return true;
                }
                return false;
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
        if !inner.accepting.load(Ordering::Acquire) {
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
    inner.idle_worker_count.fetch_add(1, Ordering::AcqRel);
}

/// Marks a fixed-pool worker as no longer idle.
///
/// # Parameters
///
/// * `inner` - Fixed pool whose idle counter is updated.
/// * `state` - Locked mutable state containing authoritative idle workers.
fn unmark_fixed_worker_idle(inner: &FixedThreadPoolInner, state: &mut FixedThreadPoolState) {
    state.idle_workers = state
        .idle_workers
        .checked_sub(1)
        .expect("fixed pool idle worker counter underflow");
    let previous = inner.idle_worker_count.fetch_sub(1, Ordering::AcqRel);
    debug_assert!(previous > 0, "fixed pool idle worker counter underflow");
    inner.consume_pending_worker_wake();
}

/// Marks one fixed-pool worker as exited.
///
/// # Parameters
///
/// * `inner` - Shared fixed-pool state.
/// * `_worker_index` - Index of the exiting worker.
fn worker_exited(inner: &FixedThreadPoolInner, _worker_index: usize) {
    inner.state.write(|state| {
        state.live_workers = state
            .live_workers
            .checked_sub(1)
            .expect("fixed pool live worker counter underflow");
    });
    inner.state.notify_all();
}
