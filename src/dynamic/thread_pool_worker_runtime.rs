/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::sync::Arc;

use crossbeam_deque::Worker;

use super::thread_pool_worker_queue::ThreadPoolWorkerQueue;
use crate::PoolJob;

/// Worker-owned local runtime for a dynamic pool worker.
pub struct ThreadPoolWorkerRuntime {
    /// Logical worker index used as a stable identity key.
    worker_index: usize,
    /// Owner-side local deque.
    local: Worker<PoolJob>,
    /// Shared stealing handle registered with the pool.
    queue: Arc<ThreadPoolWorkerQueue>,
}

impl ThreadPoolWorkerRuntime {
    /// Creates one worker-local deque and its shared stealing handle.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker owning this runtime.
    ///
    /// # Returns
    ///
    /// Worker runtime with an owner deque plus registered stealer.
    pub fn new(worker_index: usize) -> Self {
        let local = Worker::new_fifo();
        let queue = Arc::new(ThreadPoolWorkerQueue::new(worker_index, local.stealer()));
        Self {
            worker_index,
            local,
            queue,
        }
    }

    /// Returns the owning worker index.
    ///
    /// # Returns
    ///
    /// Stable worker index.
    pub fn worker_index(&self) -> usize {
        self.worker_index
    }

    /// Returns the shared queue descriptor.
    ///
    /// # Returns
    ///
    /// Clone of the registered stealer descriptor.
    pub fn queue(&self) -> Arc<ThreadPoolWorkerQueue> {
        Arc::clone(&self.queue)
    }

    /// Pops one locally owned job.
    ///
    /// # Returns
    ///
    /// `Some(job)` when the local deque contains work.
    pub fn pop_front(&self) -> Option<PoolJob> {
        self.local.pop()
    }
}
