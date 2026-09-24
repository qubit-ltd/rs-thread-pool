// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use qubit_executor::service::ExecutorServiceLifecycle;

/// Best-effort monitoring counters reported by dynamic and fixed thread pools.
///
/// Lifecycle and worker fields are read while holding the pool monitor. Task
/// counters are loaded independently from atomics and may describe adjacent,
/// rather than identical, logical instants. Use task handles, `join`, or
/// `wait_termination` instead of cross-field equalities for synchronization.
///
/// # Examples
///
/// ```
/// use qubit_thread_pool::ThreadPoolStats;
///
/// let stats = ThreadPoolStats::default();
/// assert_eq!(stats.queued_tasks, 0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadPoolStats {
    /// Observed lifecycle state.
    pub lifecycle: ExecutorServiceLifecycle,

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

    /// Number of accepted jobs completed or otherwise made inactive without
    /// queued cancellation.
    pub completed_tasks: usize,

    /// Number of accepted queued jobs cancelled by a ticket or immediate shutdown.
    pub cancelled_tasks: usize,

    /// Whether the pool has fully terminated.
    pub terminated: bool,
}

impl Default for ThreadPoolStats {
    fn default() -> Self {
        Self {
            lifecycle: ExecutorServiceLifecycle::Running,
            core_pool_size: 0,
            maximum_pool_size: 0,
            live_workers: 0,
            idle_workers: 0,
            queued_tasks: 0,
            running_tasks: 0,
            submitted_tasks: 0,
            completed_tasks: 0,
            cancelled_tasks: 0,
            terminated: false,
        }
    }
}
