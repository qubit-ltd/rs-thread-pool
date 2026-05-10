/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Worker-local queue runtime for fixed-size pools.

use std::{cell::Cell, sync::Arc};

use crossbeam_deque::Worker;

use super::fixed_worker_queue::FixedWorkerQueue;
use crate::PoolJob;

/// Worker-owned queue runtime.
///
/// The shared [`FixedWorkerQueue`] can be seen by submitters, shutdown, and thieves,
/// but only the owning worker thread may touch [`Self::local`].
pub struct FixedWorkerRuntime {
    /// Shared metadata and externally visible inbox for this worker.
    pub queue: Arc<FixedWorkerQueue>,
    /// Owner-only deque used by the worker for batched and stolen jobs.
    pub local: Worker<PoolJob>,
    /// Owner-only cursor used to rotate steal victim probing.
    steal_cursor: Cell<usize>,
}

impl FixedWorkerRuntime {
    /// Creates a worker runtime and its shared queue handle.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker owning this runtime.
    ///
    /// # Returns
    ///
    /// A runtime whose shared queue handle can be registered for submitters and
    /// thieves while its local deque remains owner-only.
    pub fn new(worker_index: usize) -> Self {
        let local = Worker::new_fifo();
        let queue = Arc::new(FixedWorkerQueue::new(worker_index, local.stealer()));
        Self {
            queue,
            local,
            steal_cursor: Cell::new(worker_index.wrapping_add(1)),
        }
    }

    /// Returns the owning worker index.
    ///
    /// # Returns
    ///
    /// Stable worker index for this runtime.
    #[inline]
    pub fn worker_index(&self) -> usize {
        self.queue.worker_index()
    }

    /// Returns the next steal-probing start index for the given queue count.
    ///
    /// # Parameters
    ///
    /// * `queue_count` - Number of currently registered worker queues.
    ///
    /// # Returns
    ///
    /// Start offset for the next victim scan.
    pub fn next_steal_start(&self, queue_count: usize) -> usize {
        let current = self.steal_cursor.get();
        self.steal_cursor.set(current.wrapping_add(1));
        current % queue_count
    }
}
