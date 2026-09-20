// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Shared admission and task accounting for thread pools.

use super::AdmissionGate;
use super::sync::AtomicUsize;
use super::sync::Ordering;

/// Admission, queue reservations, and task counters shared by both pool kinds.
pub(crate) struct PoolAccounting {
    admission: AdmissionGate,
    queue_capacity: Option<usize>,
    queue_slot_count: AtomicUsize,
    queued_task_count: AtomicUsize,
    running_task_count: AtomicUsize,
    cancelling_task_count: AtomicUsize,
    submitted_task_count: AtomicUsize,
    completed_task_count: AtomicUsize,
    cancelled_task_count: AtomicUsize,
}

/// Independently loaded counters for best-effort monitoring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PoolCounterSnapshot {
    pub(crate) inflight_submissions: usize,
    pub(crate) queued_tasks: usize,
    pub(crate) running_tasks: usize,
    pub(crate) cancelling_tasks: usize,
    pub(crate) submitted_tasks: usize,
    pub(crate) completed_tasks: usize,
    pub(crate) cancelled_tasks: usize,
}

impl PoolAccounting {
    /// Creates empty accounting with `Some(limit)` bounded slots or `None`
    /// for an unbounded queue.
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
    pub(crate) fn try_enter(&self) -> bool {
        self.admission.try_enter()
    }

    /// Leaves submission accounting and returns whether this was the last
    /// submitter. Panics if no submitter has entered.
    pub(crate) fn leave(&self) -> bool {
        self.admission.leave()
    }

    /// Rejects future entrants while allowing existing submitters to finish.
    pub(crate) fn close_admission(&self) {
        self.admission.close();
    }

    /// Returns whether new submissions may still enter.
    pub(crate) fn is_admission_open(&self) -> bool {
        self.admission.is_open()
    }

    /// Returns the number of submitters that may publish or roll back work.
    pub(crate) fn inflight_count(&self) -> usize {
        self.admission.inflight_count()
    }

    /// Returns the number of published jobs waiting to be claimed.
    pub(crate) fn queued_count(&self) -> usize {
        self.queued_task_count.load(Ordering::Acquire)
    }

    /// Returns the number of jobs currently held by workers.
    pub(crate) fn running_count(&self) -> usize {
        self.running_task_count.load(Ordering::Acquire)
    }

    /// Reserves a queue slot, returning `false` if its optional limit is full.
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
    pub(crate) fn is_idle(&self) -> bool {
        self.inflight_count() == 0
            && self.queue_slot_count.load(Ordering::Acquire) == 0
            && self.running_count() == 0
            && self.cancelling_task_count.load(Ordering::Acquire) == 0
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use std::sync::atomic::Ordering;

    use super::PoolAccounting;

    #[test]
    fn test_accepted_job_runs_to_completion() {
        let accounting = PoolAccounting::new(None);
        accounting.reserve_worker_handoff_slot();
        assert!(!accounting.is_idle());
        accounting.publish_accepted_job();
        assert_eq!(accounting.snapshot().queued_tasks, 1);
        accounting.claim_queued_job();
        assert_eq!(accounting.queue_slot_count.load(Ordering::Acquire), 0);
        assert_eq!(accounting.snapshot().running_tasks, 1);
        assert!(!accounting.is_idle());
        accounting.finish_running_job();
        let snapshot = accounting.snapshot();
        assert_eq!(snapshot.submitted_tasks, 1);
        assert_eq!(snapshot.completed_tasks, 1);
        assert_eq!(snapshot.cancelled_tasks, 0);
        assert!(accounting.is_idle());
    }

    #[test]
    fn test_cancelled_job_releases_reserved_slot() {
        let accounting = PoolAccounting::new(Some(1));
        assert!(accounting.try_reserve_bounded_slot());
        assert!(!accounting.try_reserve_bounded_slot());
        accounting.publish_accepted_job();
        accounting.begin_cancel_queued_job();
        assert_eq!(accounting.queue_slot_count.load(Ordering::Acquire), 1);
        assert_eq!(accounting.snapshot().cancelling_tasks, 1);
        assert!(!accounting.try_reserve_bounded_slot());
        assert!(!accounting.is_idle());
        accounting.finish_cancelled_job();
        assert_eq!(accounting.snapshot().cancelled_tasks, 1);
        assert_eq!(accounting.snapshot().cancelling_tasks, 0);
        assert!(accounting.is_idle());
        assert!(accounting.try_reserve_bounded_slot());
    }

    #[test]
    fn test_rollback_and_handoff_preserve_capacity() {
        let accounting = PoolAccounting::new(Some(0));
        assert!(!accounting.try_reserve_bounded_slot());
        accounting.reserve_worker_handoff_slot();
        assert_eq!(accounting.queue_slot_count.load(Ordering::Acquire), 1);
        accounting.rollback_reserved_slot();
        assert_eq!(accounting.queue_slot_count.load(Ordering::Acquire), 0);
        assert_eq!(accounting.snapshot().submitted_tasks, 0);
        assert!(accounting.is_idle());

        let unbounded = PoolAccounting::new(None);
        assert!(unbounded.try_reserve_bounded_slot());
        assert!(unbounded.try_reserve_bounded_slot());
        assert_eq!(unbounded.queue_slot_count.load(Ordering::Acquire), 2);
    }

    #[test]
    fn test_admission_remains_busy_until_last_submitter_leaves() {
        let accounting = PoolAccounting::new(None);
        assert!(accounting.try_enter());
        assert!(accounting.try_enter());
        assert_eq!(accounting.queue_slot_count.load(Ordering::Acquire), 0);
        assert_eq!(accounting.snapshot().inflight_submissions, 2);
        assert!(!accounting.is_idle());
        accounting.close_admission();
        assert!(!accounting.is_admission_open());
        assert!(!accounting.try_enter());
        assert!(!accounting.leave());
        assert!(accounting.leave());
        assert!(accounting.is_idle());
    }

    #[test]
    #[should_panic(expected = "queue slot counter underflow")]
    fn test_rollback_rejects_queue_slot_underflow() {
        PoolAccounting::new(None).rollback_reserved_slot();
    }

    #[test]
    #[should_panic(expected = "queued task counter underflow")]
    fn test_claim_rejects_queued_task_underflow() {
        PoolAccounting::new(None).claim_queued_job();
    }

    #[test]
    #[should_panic(expected = "queued task counter underflow")]
    fn test_cancel_rejects_queued_task_underflow() {
        PoolAccounting::new(None).begin_cancel_queued_job();
    }

    #[test]
    #[should_panic(expected = "running task counter underflow")]
    fn test_finish_rejects_running_task_underflow() {
        PoolAccounting::new(None).finish_running_job();
    }

    #[test]
    #[should_panic(expected = "cancelling task counter underflow")]
    fn test_finish_rejects_cancelling_task_underflow() {
        PoolAccounting::new(None).finish_cancelled_job();
    }
}

#[cfg(all(test, loom, feature = "loom-model"))]
mod loom_tests {
    use loom::sync::Arc;
    use loom::sync::atomic::Ordering;
    use loom::thread;

    use super::PoolAccounting;

    /// Two contenders cannot exceed capacity and return every reserved slot.
    #[test]
    fn loom_bounded_slot_is_reclaimed() {
        loom::model(|| {
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

    /// Cancellation retains its slot through callback accounting while another
    /// submitter competes for that capacity, then restores a balanced idle
    /// state.
    #[test]
    fn loom_cancellation_retains_slot_until_finished() {
        loom::model(|| {
            let accounting = Arc::new(PoolAccounting::new(Some(1)));
            assert!(accounting.try_reserve_bounded_slot());
            accounting.publish_accepted_job();
            let cancelling_accounting = Arc::clone(&accounting);
            let cancellation = thread::spawn(move || {
                cancelling_accounting.begin_cancel_queued_job();
                assert_eq!(cancelling_accounting.queue_slot_count.load(Ordering::Acquire), 1);
                assert!(!cancelling_accounting.is_idle());
                assert!(!cancelling_accounting.try_reserve_bounded_slot());
                cancelling_accounting.finish_cancelled_job();
            });
            if accounting.try_reserve_bounded_slot() {
                accounting.rollback_reserved_slot();
            }
            cancellation.join().expect("cancellation should finish");
            let snapshot = accounting.snapshot();
            assert_eq!(snapshot.submitted_tasks, 1);
            assert_eq!(snapshot.cancelled_tasks, 1);
            assert_eq!(snapshot.queued_tasks, 0);
            assert_eq!(snapshot.cancelling_tasks, 0);
            assert!(accounting.is_idle());
        });
    }
}
