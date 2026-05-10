/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Queue stealing adapters for fixed-pool worker queues.

use crossbeam_deque::{Injector, Steal, Stealer, Worker};

use crate::PoolJob;

/// Steals one job with immediate retry on transient contention.
///
/// # Parameters
///
/// * `source` - Queue source to probe.
///
/// # Returns
///
/// `Some(job)` when the source contains a job, otherwise `None`.
pub fn steal_one<S>(source: &S) -> Option<PoolJob>
where
    S: QueueStealSource,
{
    retry_steal(|| source.steal_one())
}

/// Steals a batch into `dest` and returns one job.
///
/// # Parameters
///
/// * `source` - Queue source that may provide one or more jobs.
/// * `dest` - Owner-local deque receiving any stolen batch remainder.
///
/// # Returns
///
/// `Some(job)` when the source or destination yields a job, otherwise `None`.
pub fn steal_batch_and_pop<S>(source: &S, dest: &Worker<PoolJob>) -> Option<PoolJob>
where
    S: QueueStealSource,
{
    retry_steal(|| source.steal_batch_and_pop(dest))
}

/// Retries transient steal contention until a stable result is observed.
///
/// # Parameters
///
/// * `steal` - Steal operation to invoke until it succeeds or reports empty.
///
/// # Returns
///
/// `Some(job)` when a job is stolen, otherwise `None` when the source is empty.
fn retry_steal<F>(mut steal: F) -> Option<PoolJob>
where
    F: FnMut() -> Steal<PoolJob>,
{
    loop {
        match steal() {
            Steal::Success(job) => return Some(job),
            Steal::Empty => return None,
            Steal::Retry => continue,
        }
    }
}

/// Small adapter trait over crossbeam steal sources used by pool queues.
pub trait QueueStealSource {
    /// Steals one job from this source.
    fn steal_one(&self) -> Steal<PoolJob>;

    /// Steals a batch into `dest` and pops one job from `dest`.
    fn steal_batch_and_pop(&self, dest: &Worker<PoolJob>) -> Steal<PoolJob>;
}

impl QueueStealSource for Injector<PoolJob> {
    #[inline]
    fn steal_one(&self) -> Steal<PoolJob> {
        self.steal()
    }

    #[inline]
    fn steal_batch_and_pop(&self, dest: &Worker<PoolJob>) -> Steal<PoolJob> {
        Injector::steal_batch_and_pop(self, dest)
    }
}

impl QueueStealSource for Stealer<PoolJob> {
    #[inline]
    fn steal_one(&self) -> Steal<PoolJob> {
        self.steal()
    }

    #[inline]
    fn steal_batch_and_pop(&self, dest: &Worker<PoolJob>) -> Steal<PoolJob> {
        Stealer::steal_batch_and_pop(self, dest)
    }
}
