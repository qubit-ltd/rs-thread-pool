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
    Injector,
    Steal,
};

use crate::PoolJob;

/// Queue owned by one worker and used for local dispatch plus stealing.
pub struct ThreadPoolWorkerQueue {
    /// Logical worker index used as a stable identity key.
    worker_index: usize,
    /// Lock-free deque of queued jobs assigned to this worker.
    jobs: Injector<PoolJob>,
}

impl ThreadPoolWorkerQueue {
    /// Creates an empty local queue for one worker.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker owning this queue.
    ///
    /// # Returns
    ///
    /// A local queue with no jobs.
    pub fn new(worker_index: usize) -> Self {
        Self {
            worker_index,
            jobs: Injector::new(),
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

    /// Appends a job to the back of this queue.
    ///
    /// # Parameters
    ///
    /// * `job` - Job to enqueue.
    pub fn push_back(&self, job: PoolJob) {
        self.jobs.push(job);
    }

    /// Pops one job from the front of this queue.
    ///
    /// # Returns
    ///
    /// `Some(job)` when this queue is non-empty, otherwise `None`.
    ///
    /// # Implementation notes
    ///
    /// [`Injector`] does not expose an owner-only pop operation. Both "local
    /// pop" and "remote steal" therefore share the same `steal()` primitive.
    pub fn pop_front(&self) -> Option<PoolJob> {
        self.steal_one()
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
            match self.jobs.steal() {
                Steal::Success(job) => return Some(job),
                Steal::Empty => return None,
                // Another thread raced us while mutating queue internals.
                // Retry immediately to mask this transient state from callers.
                Steal::Retry => continue,
            }
        }
    }
}
