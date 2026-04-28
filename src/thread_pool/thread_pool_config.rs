/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
use std::time::Duration;

/// Immutable and initial mutable configuration used by a thread pool.
pub(super) struct ThreadPoolConfig {
    /// Initial core pool size.
    pub(super) core_pool_size: usize,
    /// Initial maximum pool size.
    pub(super) maximum_pool_size: usize,
    /// Optional maximum number of queued jobs.
    pub(super) queue_capacity: Option<usize>,
    /// Prefix used for worker thread names.
    pub(super) thread_name_prefix: String,
    /// Optional stack size in bytes for worker threads.
    pub(super) stack_size: Option<usize>,
    /// Idle timeout for workers allowed to retire.
    pub(super) keep_alive: Duration,
    /// Whether idle core workers may also retire.
    pub(super) allow_core_thread_timeout: bool,
}
