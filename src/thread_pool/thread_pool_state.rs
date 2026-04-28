/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
use std::{
    collections::VecDeque,
    time::Duration,
};

use super::pool_job::PoolJob;
use super::thread_pool_config::ThreadPoolConfig;
use super::thread_pool_lifecycle::ThreadPoolLifecycle;

/// Mutable pool state protected by [`super::thread_pool_inner::ThreadPoolInner::state`].
pub(super) struct ThreadPoolState {
    /// Current lifecycle state controlling submissions and worker exits.
    pub(super) lifecycle: ThreadPoolLifecycle,
    /// Global fallback FIFO queue for accepted jobs waiting for a worker.
    ///
    /// Most jobs may be dispatched into per-worker local queues first. This
    /// global queue is kept as an injection fallback and as a migration target
    /// when workers retire.
    pub(super) queue: VecDeque<PoolJob>,
    /// Number of accepted jobs that are queued but not started yet.
    ///
    /// This includes jobs in the global queue and all per-worker local queues.
    pub(super) queued_tasks: usize,
    /// Optional maximum number of queued jobs.
    pub(super) queue_capacity: Option<usize>,
    /// Number of jobs currently held by workers.
    pub(super) running_tasks: usize,
    /// Number of worker loops that have not exited.
    pub(super) live_workers: usize,
    /// Number of live workers currently waiting for work.
    pub(super) idle_workers: usize,
    /// Total number of jobs accepted since pool creation.
    pub(super) submitted_tasks: usize,
    /// Total number of worker-held jobs completed since pool creation.
    pub(super) completed_tasks: usize,
    /// Total number of queued jobs cancelled by abrupt shutdown.
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
    /// [`ThreadPoolLifecycle::Running`], and sizing or policy fields are copied
    /// from `config`.
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
            lifecycle: ThreadPoolLifecycle::Running,
            queue: VecDeque::new(),
            queued_tasks: 0,
            queue_capacity: config.queue_capacity,
            running_tasks: 0,
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
        !self.lifecycle.is_running()
            && self.queued_tasks == 0
            && self.running_tasks == 0
            && self.live_workers == 0
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
}
