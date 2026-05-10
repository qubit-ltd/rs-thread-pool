/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use qubit_executor::service::ExecutorServiceLifecycle;

/// Mutable state protected by the fixed pool monitor.
pub struct FixedThreadPoolState {
    /// Current lifecycle state.
    pub lifecycle: ExecutorServiceLifecycle,
    /// Number of worker loops that have not exited.
    pub live_workers: usize,
    /// Number of workers currently blocked waiting for work.
    pub idle_workers: usize,
}

impl FixedThreadPoolState {
    /// Creates an empty running state.
    ///
    /// # Returns
    ///
    /// A running state before any worker has been reserved.
    pub(crate) fn new() -> Self {
        Self {
            lifecycle: ExecutorServiceLifecycle::Running,
            live_workers: 0,
            idle_workers: 0,
        }
    }
}
