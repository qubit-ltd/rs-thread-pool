/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::{collections::VecDeque, time::Duration};

use qubit_executor::service::ExecutorServiceLifecycle;

use super::thread_pool_config::ThreadPoolConfig;
use crate::{PoolJob, ThreadPoolStats};

/// Mutable pool state protected by [`super::thread_pool_inner::ThreadPoolInner::state`].
pub(crate) struct ThreadPoolState {
    /// Current lifecycle state controlling submissions and worker exits.
    pub(super) lifecycle: ExecutorServiceLifecycle,
    /// Primary FIFO queue for accepted jobs waiting for a worker.
    ///
    /// Worker-local queues may also contain jobs during migration or future
    /// local-dispatch paths. Retiring workers migrate any leftover local jobs
    /// back into this queue.
    pub(super) queue: VecDeque<PoolJob>,
    /// Number of accepted jobs that are queued but not started yet.
    ///
    /// This includes jobs in the global queue and all per-worker local queues.
    pub(super) queued_tasks: usize,
    /// Optional maximum number of queued jobs.
    pub(super) queue_capacity: Option<usize>,
    /// Number of jobs currently held by workers.
    pub(super) running_tasks: usize,
    /// Number of drained queued jobs whose cancellation callback is running.
    pub(super) cancelling_tasks: usize,
    /// Number of worker loops that have not exited.
    pub(super) live_workers: usize,
    /// Number of live workers currently waiting for work.
    pub(super) idle_workers: usize,
    /// Total number of jobs accepted since pool creation.
    pub(super) submitted_tasks: usize,
    /// Total number of accepted jobs completed or otherwise made inactive
    /// without queued cancellation.
    pub(super) completed_tasks: usize,
    /// Total number of queued jobs selected for abrupt-shutdown cancellation.
    pub(super) cancelled_tasks: usize,
    /// Current configured core pool size.
    pub(super) core_pool_size: usize,
    /// Current configured maximum pool size.
    pub(super) maximum_pool_size: usize,
    /// Current idle timeout for workers allowed to retire.
    pub(super) keep_alive: Duration,
    /// Whether core workers are allowed to time out while idle.
    pub(super) allow_core_thread_timeout: bool,
    /// Index assigned to the next spawned worker.
    pub(super) next_worker_index: usize,
}

impl ThreadPoolState {
    /// Builds the initial mutex-protected pool state for a newly created pool.
    ///
    /// Counter fields start at zero, the job queue is empty, the lifecycle is
    /// [`ExecutorServiceLifecycle::Running`], and sizing or policy fields are
    /// copied from `config`.
    ///
    /// # Parameters
    ///
    /// * `config` - Full [`ThreadPoolConfig`]; this constructor reads
    ///   `queue_capacity`, `core_pool_size`, `maximum_pool_size`, `keep_alive`,
    ///   and `allow_core_thread_timeout`. It does not read `thread_name_prefix`
    ///   or `stack_size`.
    ///
    /// # Returns
    ///
    /// A [`ThreadPoolState`] ready to be wrapped by
    /// [`ThreadPoolInner::state`](super::thread_pool_inner::ThreadPoolInner::state).
    ///
    /// # Note
    ///
    /// [`ThreadPoolInner::new`](super::thread_pool_inner::ThreadPoolInner::new)
    /// takes ownership of `config` for this call but must keep the thread name
    /// prefix and stack size for spawning workers; it typically
    /// [`std::mem::take`]s `thread_name_prefix` and copies `stack_size` before
    /// passing the remaining `config` here, so the prefix field in the moved
    /// value may be empty and is ignored.
    pub(super) fn new(config: ThreadPoolConfig) -> Self {
        Self {
            lifecycle: ExecutorServiceLifecycle::Running,
            queue: VecDeque::new(),
            queued_tasks: 0,
            queue_capacity: config.queue_capacity,
            running_tasks: 0,
            cancelling_tasks: 0,
            live_workers: 0,
            idle_workers: 0,
            submitted_tasks: 0,
            completed_tasks: 0,
            cancelled_tasks: 0,
            core_pool_size: config.core_pool_size,
            maximum_pool_size: config.maximum_pool_size,
            keep_alive: config.keep_alive,
            allow_core_thread_timeout: config.allow_core_thread_timeout,
            next_worker_index: 0,
        }
    }

    /// Returns whether the queue is currently full.
    ///
    /// # Returns
    ///
    /// `true` when the queue has a configured capacity and has reached it.
    pub(super) fn is_saturated(&self) -> bool {
        self.queue_capacity
            .is_some_and(|capacity| self.queued_tasks >= capacity)
    }

    /// Returns whether the service lifecycle is fully terminated.
    ///
    /// # Returns
    ///
    /// `true` after shutdown has started, the queue is empty, no jobs are
    /// running, and no workers remain live.
    pub(super) fn is_terminated(&self) -> bool {
        self.lifecycle != ExecutorServiceLifecycle::Running
            && self.queued_tasks == 0
            && self.running_tasks == 0
            && self.cancelling_tasks == 0
            && self.live_workers == 0
    }

    /// Returns whether no accepted work is queued or running.
    ///
    /// # Returns
    ///
    /// `true` when all currently accepted tasks have completed or been
    /// cancelled, regardless of whether worker threads remain alive.
    pub(super) fn is_idle(&self) -> bool {
        self.queued_tasks == 0 && self.running_tasks == 0 && self.cancelling_tasks == 0
    }

    /// Returns whether an idle worker should use a timed wait.
    ///
    /// # Returns
    ///
    /// `true` when core timeout is enabled or the live worker count exceeds
    /// the core pool size.
    pub(super) fn worker_wait_is_timed(&self) -> bool {
        self.allow_core_thread_timeout || self.live_workers > self.core_pool_size
    }

    /// Returns whether an idle worker may retire now.
    ///
    /// # Returns
    ///
    /// `true` when the worker count exceeds the maximum size, or when timeout
    /// policy allows an idle worker to exit.
    pub(super) fn idle_worker_can_retire(&self) -> bool {
        self.live_workers > self.maximum_pool_size
            || (self.worker_wait_is_timed()
                && (self.live_workers > self.core_pool_size || self.allow_core_thread_timeout))
    }

    /// Builds a point-in-time stats snapshot from this locked state.
    ///
    /// # Returns
    ///
    /// A [`ThreadPoolStats`] snapshot for monitoring and tests.
    pub(super) fn stats(&self) -> ThreadPoolStats {
        ThreadPoolStats {
            lifecycle: if self.is_terminated() {
                ExecutorServiceLifecycle::Terminated
            } else {
                self.lifecycle
            },
            core_pool_size: self.core_pool_size,
            maximum_pool_size: self.maximum_pool_size,
            live_workers: self.live_workers,
            idle_workers: self.idle_workers,
            queued_tasks: self.queued_tasks,
            running_tasks: self.running_tasks,
            submitted_tasks: self.submitted_tasks,
            completed_tasks: self.completed_tasks,
            cancelled_tasks: self.cancelled_tasks,
            terminated: self.is_terminated(),
        }
    }
}
