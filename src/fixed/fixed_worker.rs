/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::sync::{Arc, atomic::Ordering};

use qubit_executor::service::ExecutorServiceLifecycle;

use super::fixed_thread_pool_inner::FixedThreadPoolInner;
use super::fixed_thread_pool_state::FixedThreadPoolState;
use super::fixed_worker_queue::FixedWorkerQueue;
use super::fixed_worker_runtime::FixedWorkerRuntime;

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
        worker_runtime.queue.activate();
        loop {
            if let Some(job) = inner.try_take_job(&worker_runtime) {
                job.run();
                inner.finish_running_job();
                continue;
            }
            if !wait_for_fixed_pool_work(&inner) {
                break;
            }
        }
        worker_exited(&inner, &worker_runtime.queue);
    }
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
                if inner.queued_count() == 0 && inner.inflight_count() == 0 {
                    return false;
                }
                mark_fixed_worker_idle(inner, &mut state);
                if inner.queued_count() > 0
                    || inner.inflight_count() == 0
                    || inner.has_pending_worker_wake()
                {
                    unmark_fixed_worker_idle(inner, &mut state);
                    continue;
                }
                state = state.wait();
                unmark_fixed_worker_idle(inner, &mut state);
            }
            ExecutorServiceLifecycle::Stopping => return false,
            ExecutorServiceLifecycle::Terminated => return false,
        }
    }
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
/// * `worker_queue` - Queue owned by the exiting worker.
fn worker_exited(inner: &FixedThreadPoolInner, worker_queue: &FixedWorkerQueue) {
    worker_queue.deactivate();
    inner.state.write(|state| {
        state.live_workers = state
            .live_workers
            .checked_sub(1)
            .expect("fixed pool live worker counter underflow");
    });
    inner.state.notify_all();
}
