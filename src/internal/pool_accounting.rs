// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Shared admission and task accounting for thread pools.

use super::AdmissionGate;
use super::pool_counter_snapshot::PoolCounterSnapshot;
use super::sync::AtomicUsize;
use super::sync::Ordering;

/// Admission, queue reservations, and task counters shared by both pool kinds.
pub(crate) struct PoolAccounting {
    /// Coordinates admission with concurrent submitters.
    admission: AdmissionGate,
    /// Limits reserved queue slots, or leaves the queue unbounded.
    queue_capacity: Option<usize>,
    /// Counts queue reservations, including worker handoffs and cancellations.
    queue_slot_count: AtomicUsize,
    /// Counts published jobs that have not been claimed.
    queued_task_count: AtomicUsize,
    /// Counts jobs currently owned by workers.
    running_task_count: AtomicUsize,
    /// Counts claimed jobs whose cancellation callbacks are still running.
    cancelling_task_count: AtomicUsize,
    /// Counts all accepted jobs.
    submitted_task_count: AtomicUsize,
    /// Counts accepted jobs completed by workers.
    completed_task_count: AtomicUsize,
    /// Counts accepted jobs whose cancellation callbacks completed.
    cancelled_task_count: AtomicUsize,
}

impl PoolAccounting {
    /// Creates empty accounting with `Some(limit)` bounded slots or `None`
    /// for an unbounded queue.
    ///
    /// # Parameters
    ///
    /// * `queue_capacity` - Optional maximum number of queued jobs.
    #[must_use]
    pub(crate) fn new(queue_capacity: Option<usize>) -> Self {
        Self {
            admission: AdmissionGate::new(),
            queue_capacity,
            queue_slot_count: AtomicUsize::new(0),
            queued_task_count: AtomicUsize::new(0),
            running_task_count: AtomicUsize::new(0),
            cancelling_task_count: AtomicUsize::new(0),
            submitted_task_count: AtomicUsize::new(0),
            completed_task_count: AtomicUsize::new(0),
            cancelled_task_count: AtomicUsize::new(0),
        }
    }

    /// Enters submission accounting, returning `false` after admission closes.
    #[must_use]
    #[inline]
    pub(crate) fn try_enter(&self) -> bool {
        self.admission.try_enter()
    }

    /// Leaves submission accounting and returns whether this was the last
    /// submitter. Panics if no submitter has entered.
    #[must_use]
    #[inline]
    pub(crate) fn leave(&self) -> bool {
        self.admission.leave()
    }

    /// Rejects future entrants while allowing existing submitters to finish.
    #[inline]
    pub(crate) fn close_admission(&self) {
        self.admission.close();
    }

    /// Returns whether new submissions may still enter.
    #[must_use]
    #[inline]
    pub(crate) fn is_admission_open(&self) -> bool {
        self.admission.is_open()
    }

    /// Returns the number of submitters that may publish or roll back work.
    #[must_use]
    #[inline]
    pub(crate) fn inflight_count(&self) -> usize {
        self.admission.inflight_count()
    }

    /// Returns the number of published jobs waiting to be claimed.
    #[must_use]
    #[inline]
    pub(crate) fn queued_count(&self) -> usize {
        self.queued_task_count.load(Ordering::Acquire)
    }

    /// Returns the number of jobs currently held by workers.
    #[must_use]
    #[inline]
    pub(crate) fn running_count(&self) -> usize {
        self.running_task_count.load(Ordering::Acquire)
    }

    /// Returns the configured queue limit, or `None` for an unbounded queue.
    #[must_use]
    #[inline]
    pub(crate) fn queue_capacity(&self) -> Option<usize> {
        self.queue_capacity
    }

    /// Reserves a queue slot, returning `false` if its optional limit is full.
    #[must_use]
    #[inline]
    pub(crate) fn try_reserve_bounded_slot(&self) -> bool {
        if let Some(capacity) = self.queue_capacity {
            return self
                .queue_slot_count
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    (current < capacity).then_some(current + 1)
                })
                .is_ok();
        }
        self.reserve_worker_handoff_slot();
        true
    }

    /// Reserves a job for a newly spawned worker regardless of queue capacity.
    #[inline]
    pub(crate) fn reserve_worker_handoff_slot(&self) {
        self.queue_slot_count.fetch_add(1, Ordering::Release);
    }

    /// Releases a reservation whose acceptance failed.
    /// Panics if no queue slot is reserved.
    pub(crate) fn rollback_reserved_slot(&self) {
        let previous = self.queue_slot_count.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "queue slot counter underflow");
    }

    /// Records an accepted job before the caller makes it visible to workers.
    /// The caller must already own a queue slot.
    pub(crate) fn publish_accepted_job(&self) {
        self.submitted_task_count.fetch_add(1, Ordering::Release);
        self.queued_task_count.fetch_add(1, Ordering::Release);
    }

    /// Transfers a claimed queued job to running accounting and releases its
    /// queue slot. Panics if no queued job or reserved slot exists.
    pub(crate) fn claim_queued_job(&self) {
        let previous = self.queued_task_count.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "queued task counter underflow");
        // Publish running ownership before releasing the reservation so idle
        // checks cannot observe a gap between queued and running work.
        self.running_task_count.fetch_add(1, Ordering::Release);
        self.rollback_reserved_slot();
    }

    /// Starts cancellation of a claimed queued job, retaining its slot until
    /// the callback finishes. Panics if no queued job exists.
    pub(crate) fn begin_cancel_queued_job(&self) {
        let previous = self.queued_task_count.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "queued task counter underflow");
        self.cancelling_task_count.fetch_add(1, Ordering::Release);
    }

    /// Records a completed cancellation callback and releases its slot.
    /// Panics if no cancellation or reserved slot exists.
    pub(crate) fn finish_cancelled_job(&self) {
        self.cancelled_task_count.fetch_add(1, Ordering::Release);
        let previous = self.cancelling_task_count.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "cancelling task counter underflow");
        self.rollback_reserved_slot();
    }

    /// Records completion of a worker-held job.
    /// Panics if no job is running.
    pub(crate) fn finish_running_job(&self) {
        self.completed_task_count.fetch_add(1, Ordering::Release);
        let previous = self.running_task_count.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "running task counter underflow");
    }

    /// Loads task counters independently; cross-field equalities are not
    /// synchronization guarantees while transitions are in progress.
    #[must_use]
    pub(crate) fn snapshot(&self) -> PoolCounterSnapshot {
        PoolCounterSnapshot {
            inflight_submissions: self.inflight_count(),
            queued_tasks: self.queued_count(),
            running_tasks: self.running_count(),
            cancelling_tasks: self.cancelling_task_count.load(Ordering::Acquire),
            submitted_tasks: self.submitted_task_count.load(Ordering::Acquire),
            completed_tasks: self.completed_task_count.load(Ordering::Acquire),
            cancelled_tasks: self.cancelled_task_count.load(Ordering::Acquire),
        }
    }

    /// Returns whether no submitter, reserved slot, running job, or
    /// cancellation callback remains. New submissions may invalidate this
    /// observation.
    #[must_use]
    pub(crate) fn is_idle(&self) -> bool {
        self.inflight_count() == 0
            && self.queue_slot_count.load(Ordering::Acquire) == 0
            && self.running_count() == 0
            && self.cancelling_task_count.load(Ordering::Acquire) == 0
    }
}

#[cfg(all(test, loom, feature = "loom-model"))]
mod loom_tests {
    use loom::model;
    use loom::sync::Arc;
    use loom::sync::atomic::Ordering;
    use loom::thread;

    use super::PoolAccounting;

    /// Two contenders cannot exceed capacity and return every reserved slot.
    #[test]
    fn test_loom_bounded_slot_is_reclaimed() {
        model(|| {
            let accounting = Arc::new(PoolAccounting::new(Some(1)));
            let first_accounting = Arc::clone(&accounting);
            let second_accounting = Arc::clone(&accounting);
            let first = thread::spawn(move || {
                if first_accounting.try_reserve_bounded_slot() {
                    assert_eq!(first_accounting.queue_slot_count.load(Ordering::Acquire), 1);
                    first_accounting.rollback_reserved_slot();
                }
            });
            let second = thread::spawn(move || {
                if second_accounting.try_reserve_bounded_slot() {
                    assert_eq!(second_accounting.queue_slot_count.load(Ordering::Acquire), 1);
                    second_accounting.rollback_reserved_slot();
                }
            });
            first.join().expect("first contender should finish");
            second.join().expect("second contender should finish");
            assert_eq!(accounting.queue_slot_count.load(Ordering::Acquire), 0);
            assert!(accounting.is_idle());
            assert!(accounting.try_reserve_bounded_slot());
            accounting.rollback_reserved_slot();
        });
    }
}
