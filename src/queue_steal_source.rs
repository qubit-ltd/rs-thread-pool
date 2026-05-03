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

use crossbeam_deque::{
    Injector,
    Steal,
    Stealer,
    Worker,
};

use crate::thread_pool::PoolJob;

/// Steals one job with immediate retry on transient contention.
///
/// # Parameters
///
/// * `source` - Queue source to probe.
///
/// # Returns
///
/// `Some(job)` when the source contains a job, otherwise `None`.
pub(crate) fn steal_one<S>(source: &S) -> Option<PoolJob>
where
    S: QueueStealSource,
{
    loop {
        match source.steal_one() {
            Steal::Success(job) => return Some(job),
            Steal::Empty => return None,
            Steal::Retry => continue,
        }
    }
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
pub(crate) fn steal_batch_and_pop<S>(source: &S, dest: &Worker<PoolJob>) -> Option<PoolJob>
where
    S: QueueStealSource,
{
    loop {
        match source.steal_batch_and_pop(dest) {
            Steal::Success(job) => return Some(job),
            Steal::Empty => return None,
            Steal::Retry => continue,
        }
    }
}

/// Small adapter trait over crossbeam steal sources used by pool queues.
pub(crate) trait QueueStealSource {
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

// qubit-style: allow coverage-cfg
#[cfg(coverage)]
pub mod coverage_support {
    //! Coverage-only helpers for defensive queue stealing branches.

    use std::sync::atomic::{
        AtomicUsize,
        Ordering,
    };

    use crossbeam_deque::{
        Steal,
        Worker,
    };

    use super::{
        QueueStealSource,
        steal_batch_and_pop,
        steal_one,
    };
    use crate::{
        thread_pool::PoolJob,
        worker_runtime::WorkerRuntime,
    };

    /// Fake steal source that reports one retry before returning a job.
    struct RetryOnceSource {
        /// Number of `steal_one` calls observed by this source.
        steal_one_calls: AtomicUsize,
        /// Number of `steal_batch_and_pop` calls observed by this source.
        batch_calls: AtomicUsize,
    }

    impl RetryOnceSource {
        /// Creates a fake source with no recorded calls.
        fn new() -> Self {
            Self {
                steal_one_calls: AtomicUsize::new(0),
                batch_calls: AtomicUsize::new(0),
            }
        }
    }

    impl QueueStealSource for RetryOnceSource {
        /// Returns `Retry` on the first call and a job on the second call.
        fn steal_one(&self) -> Steal<PoolJob> {
            if self.steal_one_calls.fetch_add(1, Ordering::AcqRel) == 0 {
                Steal::Retry
            } else {
                Steal::Success(noop_job())
            }
        }

        /// Returns `Retry` on the first call and a job on the second call.
        fn steal_batch_and_pop(&self, _dest: &Worker<PoolJob>) -> Steal<PoolJob> {
            if self.batch_calls.fetch_add(1, Ordering::AcqRel) == 0 {
                Steal::Retry
            } else {
                Steal::Success(noop_job())
            }
        }
    }

    /// Creates a job with no observable side effects.
    fn noop_job() -> PoolJob {
        PoolJob::new(Box::new(|| {}), Box::new(|| {}))
    }

    /// Exercises all coverage-only defensive branches in this module.
    ///
    /// # Returns
    ///
    /// Labels for the exercised branch groups.
    pub fn exercise_all() -> Vec<&'static str> {
        exercise_retry_paths();
        exercise_worker_queue_drain_paths();
        vec!["queue-steal-retry", "worker-queue-drain"]
    }

    /// Exercises retry handling for both stealing helpers.
    fn exercise_retry_paths() {
        let source = RetryOnceSource::new();

        let job = steal_one(&source).expect("retrying source should eventually provide a job");
        job.run();
        assert_eq!(source.steal_one_calls.load(Ordering::Acquire), 2);

        let dest = Worker::new_fifo();
        let job = steal_batch_and_pop(&source, &dest)
            .expect("retrying source should eventually provide a job");
        job.cancel();
        assert_eq!(source.batch_calls.load(Ordering::Acquire), 2);
    }

    /// Exercises worker queue draining through both local stealer and inbox paths.
    fn exercise_worker_queue_drain_paths() {
        let runtime = WorkerRuntime::new(0);
        runtime.local.push(noop_job());
        runtime.queue.push_back(noop_job());

        let jobs = runtime.queue.drain();

        assert_eq!(jobs.len(), 2);
        for (index, job) in jobs.into_iter().enumerate() {
            if index == 0 {
                job.run();
            } else {
                job.cancel();
            }
        }
    }
}
