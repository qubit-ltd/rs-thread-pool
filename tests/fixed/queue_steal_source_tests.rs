/*******************************************************************************
 *
 *    Copyright (c) 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for queue stealing and worker queue primitives.

use std::sync::atomic::{
    AtomicUsize,
    Ordering,
};

use crossbeam_deque::{
    Injector,
    Steal,
    Worker,
};
use qubit_thread_pool::PoolJob;
use qubit_thread_pool::fixed::{
    fixed_worker_queue::FixedWorkerQueue,
    queue_steal_source::{
        QueueStealSource,
        steal_batch_and_pop,
        steal_one,
    },
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

#[test]
fn test_steal_one_retries_transient_contention() {
    let source = RetryOnceSource::new();

    let job = steal_one(&source).expect("retrying source should eventually provide a job");

    drop(job);
    assert_eq!(source.steal_one_calls.load(Ordering::Acquire), 2);
}

#[test]
fn test_steal_batch_and_pop_retries_transient_contention() {
    let source = RetryOnceSource::new();
    let dest = Worker::new_fifo();

    let job = steal_batch_and_pop(&source, &dest)
        .expect("retrying source should eventually provide a job");

    drop(job);
    assert_eq!(source.batch_calls.load(Ordering::Acquire), 2);
}

#[test]
fn test_injector_and_stealer_sources_report_empty_and_success() {
    let injector = Injector::new();
    assert!(steal_one(&injector).is_none());

    injector.push(noop_job());
    assert!(steal_one(&injector).is_some());

    let local = Worker::new_fifo();
    let stealer = local.stealer();
    assert!(steal_one(&stealer).is_none());

    local.push(noop_job());
    assert!(steal_one(&stealer).is_some());
}

#[test]
fn test_fixed_worker_queue_exposes_state_transitions_and_routes_jobs() {
    let local = Worker::new_fifo();
    let queue = FixedWorkerQueue::new(7, local.stealer());

    assert_eq!(queue.worker_index(), 7);
    assert!(!queue.is_active());
    assert!(queue.activate());
    assert!(!queue.activate());
    assert!(queue.is_active());
    assert!(queue.deactivate());
    assert!(!queue.deactivate());
    assert!(!queue.is_active());

    queue.push_back(noop_job());
    assert!(queue.pop_inbox_into(&local).is_some());
}

#[test]
fn test_fixed_worker_queue_steals_from_local_queue_and_inbox() {
    let victim_local = Worker::new_fifo();
    let victim = FixedWorkerQueue::new(1, victim_local.stealer());
    let thief_local = Worker::new_fifo();

    victim_local.push(noop_job());
    assert!(victim.steal_into(&thief_local).is_some());

    victim.push_back(noop_job());
    assert!(victim.steal_into(&thief_local).is_some());

    let empty_local = Worker::new_fifo();
    let empty = FixedWorkerQueue::new(2, empty_local.stealer());
    assert!(empty.steal_into(&thief_local).is_none());
}

#[test]
fn test_fixed_worker_queue_drain_collects_local_stealer_and_inbox_jobs() {
    let local = Worker::new_fifo();
    let queue = FixedWorkerQueue::new(0, local.stealer());
    local.push(noop_job());
    queue.push_back(noop_job());

    let jobs = queue.drain();

    assert_eq!(jobs.len(), 2);
}
