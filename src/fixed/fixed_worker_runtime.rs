/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Worker runtime identity for fixed-size pools.

/// Worker-owned runtime metadata.
pub struct FixedWorkerRuntime {
    /// Stable worker index.
    worker_index: usize,
}

impl FixedWorkerRuntime {
    /// Creates runtime metadata for one fixed-pool worker.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker owning this runtime.
    ///
    /// # Returns
    ///
    /// Runtime metadata for the worker loop.
    pub fn new(worker_index: usize) -> Self {
        Self { worker_index }
    }

    /// Returns the owning worker index.
    ///
    /// # Returns
    ///
    /// Stable worker index for this runtime.
    #[inline]
    pub fn worker_index(&self) -> usize {
        self.worker_index
    }
}
