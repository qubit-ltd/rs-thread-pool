/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Worker-local queue primitives shared by thread-pool implementations.

use std::sync::atomic::{
    AtomicBool,
    Ordering,
};

use crossbeam_deque::{
    Injector,
    Stealer,
    Worker,
};

use super::queue_steal_source::{
    QueueStealSource,
    steal_batch_and_pop,
    steal_one,
};
use crate::PoolJob;

/// Queue owned by one worker and used for local dispatch plus stealing.
pub struct FixedWorkerQueue {
    /// Logical worker index used as a stable identity key.
    worker_index: usize,
    /// Cross-thread inbox used by submitters to route work to this worker.
    inbox: Injector<PoolJob>,
    /// Stealer half of the worker-owned local deque.
    stealer: Stealer<PoolJob>,
    /// Whether this queue belongs to a worker that has reached run-loop start.
    active: AtomicBool,
}

impl FixedWorkerQueue {
    /// Creates an empty shared queue handle for one worker.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker owning this queue.
    /// * `stealer` - Read-only stealing handle for the owner-local deque.
    ///
    /// # Returns
    ///
    /// A shared queue handle with an empty cross-thread inbox.
    pub fn new(worker_index: usize, stealer: Stealer<PoolJob>) -> Self {
        Self {
            worker_index,
            inbox: Injector::new(),
            stealer,
            active: AtomicBool::new(false),
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

    /// Returns whether this queue is currently active.
    ///
    /// # Returns
    ///
    /// `true` when the owning worker has started its run loop.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Marks this queue as active after worker run-loop start.
    ///
    /// # Returns
    ///
    /// `true` when this call performed the state transition.
    pub fn activate(&self) -> bool {
        self.active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Marks this queue as inactive when the worker exits.
    ///
    /// # Returns
    ///
    /// `true` when this call performed the state transition.
    pub fn deactivate(&self) -> bool {
        self.active
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Appends a job to the worker's cross-thread inbox.
    ///
    /// # Parameters
    ///
    /// * `job` - Job to enqueue.
    pub fn push_back(&self, job: PoolJob) {
        self.inbox.push(job);
    }

    /// Pops one job from this worker's cross-thread inbox into its local deque.
    ///
    /// # Parameters
    ///
    /// * `local` - Owner-local deque receiving any stolen batch remainder.
    ///
    /// # Returns
    ///
    /// `Some(job)` when the inbox or destination local deque provides a job,
    /// otherwise `None`.
    pub fn pop_inbox_into(&self, local: &Worker<PoolJob>) -> Option<PoolJob> {
        steal_batch_and_pop(&self.inbox, local)
    }

    /// Steals one job from this worker's local deque or inbox into `dest`.
    ///
    /// # Parameters
    ///
    /// * `dest` - Owner-local deque receiving any stolen batch remainder.
    ///
    /// # Returns
    ///
    /// `Some(job)` when the victim queue provides a job, otherwise `None`.
    pub fn steal_into(&self, dest: &Worker<PoolJob>) -> Option<PoolJob> {
        steal_batch_and_pop(&self.stealer, dest).or_else(|| steal_batch_and_pop(&self.inbox, dest))
    }

    /// Drains all queued jobs from this queue.
    ///
    /// # Returns
    ///
    /// A vector containing all queued jobs currently visible through this
    /// queue's local stealer and inbox.
    pub fn drain(&self) -> Vec<PoolJob> {
        let mut jobs = Vec::new();
        drain_source(&self.stealer, &mut jobs);
        drain_source(&self.inbox, &mut jobs);
        jobs
    }
}

/// Drains every currently visible job from one steal source.
///
/// # Parameters
///
/// * `source` - Queue source to drain.
/// * `jobs` - Destination for drained jobs.
fn drain_source<S>(source: &S, jobs: &mut Vec<PoolJob>)
where
    S: QueueStealSource,
{
    while let Some(job) = steal_one(source) {
        jobs.push(job);
    }
}
