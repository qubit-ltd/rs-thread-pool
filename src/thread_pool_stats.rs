/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
/// Point-in-time counters reported by [`crate::ThreadPool`].
///
/// The snapshot is intended for monitoring and tests. It is not a stable
/// synchronization primitive; concurrent submissions and completions may make
/// the next snapshot different immediately after this one is returned.
///
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
