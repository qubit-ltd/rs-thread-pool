/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use crossbeam_deque::{
    Steal,
    Stealer,
};

use crate::PoolJob;

/// Queue owned by one worker and used for local dispatch plus stealing.
pub struct ThreadPoolWorkerQueue {
    /// Logical worker index used as a stable identity key.
    worker_index: usize,
    /// Stealing handle for the worker-owned deque.
    stealer: Stealer<PoolJob>,
}

impl ThreadPoolWorkerQueue {
    /// Creates a steal handle for one worker-owned local queue.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker owning this queue.
    /// * `stealer` - Stealing handle created from the worker-owned deque.
    ///
    /// # Returns
    ///
    /// A shared queue descriptor for the worker.
    pub fn new(worker_index: usize, stealer: Stealer<PoolJob>) -> Self {
        Self {
            worker_index,
            stealer,
        }
    }

    /// Returns the owning worker index.
    ///
    /// # Returns
    ///
    /// The worker index associated with this queue.
    #[inline]
    pub fn worker_index(&self) -> usize {
        self.worker_index
    }

    /// Steals one job from the back of this queue.
    ///
    /// # Returns
    ///
    /// `Some(job)` when this queue is non-empty, otherwise `None`.
    ///
    /// # Implementation notes
    ///
    /// This method intentionally reuses `steal_one` so all queue
    /// consumers handle transient `Steal::Retry` states consistently.
    pub fn steal_back(&self) -> Option<PoolJob> {
        self.steal_one()
    }

    /// Drains all queued jobs from this queue.
    ///
    /// # Returns
    ///
    /// A vector containing all queued jobs in FIFO order.
    pub fn drain(&self) -> Vec<PoolJob> {
        let mut jobs = Vec::new();
        while let Some(job) = self.steal_one() {
            jobs.push(job);
        }
        jobs
    }

    /// Steals one job from this queue and transparently retries transient
    /// contention states.
    ///
    /// # Returns
    ///
    /// `Some(job)` when a job is available, otherwise `None`.
    fn steal_one(&self) -> Option<PoolJob> {
        loop {
            match self.stealer.steal() {
                Steal::Success(job) => return Some(job),
                Steal::Empty => return None,
                // Another thread raced us while mutating queue internals.
                // Retry immediately to mask this transient state from callers.
                Steal::Retry => continue,
            }
        }
    }
}
