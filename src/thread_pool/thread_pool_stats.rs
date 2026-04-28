/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
/// Point-in-time counters reported by [`super::thread_pool::ThreadPool`].
///
/// The snapshot is intended for monitoring and tests. It is not a stable
/// synchronization primitive; concurrent submissions and completions may make
/// the next snapshot different immediately after this one is returned.
///
/// # Author
///
/// Haixing Hu
use super::thread_pool_state::ThreadPoolState;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ThreadPoolStats {
    /// Configured core pool size.
    pub core_pool_size: usize,

    /// Configured maximum pool size.
    pub maximum_pool_size: usize,

    /// Number of live worker loops.
    pub live_workers: usize,

    /// Number of workers currently waiting for work.
    pub idle_workers: usize,

    /// Number of queued tasks waiting for a worker.
    pub queued_tasks: usize,

    /// Number of tasks currently held by workers.
    pub running_tasks: usize,

    /// Number of tasks accepted since pool creation.
    pub submitted_tasks: usize,

    /// Number of worker-held jobs finished since pool creation.
    pub completed_tasks: usize,

    /// Number of queued jobs cancelled by immediate shutdown.
    pub cancelled_tasks: usize,

    /// Whether shutdown has been requested.
    pub shutdown: bool,

    /// Whether the pool has fully terminated.
    pub terminated: bool,
}

impl ThreadPoolStats {
    /// Builds a snapshot from locked pool state.
    ///
    /// # Parameters
    ///
    /// * `state` - Current [`ThreadPoolState`] while the pool monitor is held.
    ///
    /// # Returns
    ///
    /// A point-in-time [`ThreadPoolStats`] snapshot.
    pub(super) fn new(state: &ThreadPoolState) -> Self {
        Self {
            core_pool_size: state.core_pool_size,
            maximum_pool_size: state.maximum_pool_size,
            live_workers: state.live_workers,
            idle_workers: state.idle_workers,
            queued_tasks: state.queued_tasks,
            running_tasks: state.running_tasks,
            submitted_tasks: state.submitted_tasks,
            completed_tasks: state.completed_tasks,
            cancelled_tasks: state.cancelled_tasks,
            shutdown: !state.lifecycle.is_running(),
            terminated: state.is_terminated(),
        }
    }
}
