// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================

use crate::internal::PoolAccounting;

#[test]
fn test_accepted_job_runs_to_completion() {
    let accounting = PoolAccounting::new(Some(1));
    assert!(accounting.try_reserve_bounded_slot());
    assert!(!accounting.is_idle());
    accounting.publish_accepted_job();
    assert_eq!(accounting.snapshot().queued_tasks, 1);
    accounting.claim_queued_job();
    assert!(accounting.try_reserve_bounded_slot());
    accounting.rollback_reserved_slot();
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
    let zero_capacity = PoolAccounting::new(Some(0));
    assert!(!zero_capacity.try_reserve_bounded_slot());
    assert!(zero_capacity.is_idle());

    let accounting = PoolAccounting::new(Some(1));
    accounting.reserve_worker_handoff_slot();
    assert!(!accounting.try_reserve_bounded_slot());
    accounting.rollback_reserved_slot();
    assert!(accounting.try_reserve_bounded_slot());
    accounting.rollback_reserved_slot();
    assert_eq!(accounting.snapshot().submitted_tasks, 0);
    assert!(accounting.is_idle());

    let unbounded = PoolAccounting::new(None);
    assert!(unbounded.try_reserve_bounded_slot());
    assert!(unbounded.try_reserve_bounded_slot());
    assert!(!unbounded.is_idle());
    unbounded.rollback_reserved_slot();
    unbounded.rollback_reserved_slot();
    assert!(unbounded.is_idle());
}

#[test]
fn test_admission_remains_busy_until_last_submitter_leaves() {
    let accounting = PoolAccounting::new(None);
    assert!(accounting.try_enter());
    assert!(accounting.try_enter());
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

#[cfg(all(loom, feature = "loom-model"))]
mod loom_tests {
    use std::sync::Arc as StdArc;
    use std::sync::atomic::AtomicUsize as StdAtomicUsize;
    use std::sync::atomic::Ordering as StdOrdering;

    use loom::model;
    use loom::sync::Arc;
    use loom::thread;

    use crate::internal::PoolAccounting;

    /// Cancellation retains its slot through callback accounting while another
    /// submitter competes for that capacity, then restores a balanced idle
    /// state.
    #[test]
    fn test_loom_cancellation_retains_slot_until_finished() {
        // These counters observe completed branches across model executions;
        // they never synchronize modeled threads or influence their decisions.
        let accepted = StdArc::new(StdAtomicUsize::new(0));
        let rejected = StdArc::new(StdAtomicUsize::new(0));
        let accepted_witness = StdArc::clone(&accepted);
        let rejected_witness = StdArc::clone(&rejected);
        model(move || {
            let accounting = Arc::new(PoolAccounting::new(Some(1)));
            assert!(accounting.try_reserve_bounded_slot());
            accounting.publish_accepted_job();
            accounting.begin_cancel_queued_job();
            let cancelling_accounting = Arc::clone(&accounting);
            let cancellation = thread::spawn(move || {
                assert!(!cancelling_accounting.is_idle());
                assert!(!cancelling_accounting.try_reserve_bounded_slot());
                cancelling_accounting.finish_cancelled_job();
            });
            let competing_accounting = Arc::clone(&accounting);
            let accepted_witness = StdArc::clone(&accepted_witness);
            let rejected_witness = StdArc::clone(&rejected_witness);
            let contender = thread::spawn(move || {
                if competing_accounting.try_reserve_bounded_slot() {
                    let snapshot = competing_accounting.snapshot();
                    assert_eq!(snapshot.cancelled_tasks, 1);
                    assert_eq!(snapshot.cancelling_tasks, 0);
                    accepted_witness.fetch_add(1, StdOrdering::Relaxed);
                    competing_accounting.rollback_reserved_slot();
                } else {
                    rejected_witness.fetch_add(1, StdOrdering::Relaxed);
                }
            });
            cancellation.join().expect("cancellation should finish");
            contender.join().expect("contender should finish");
            let snapshot = accounting.snapshot();
            assert_eq!(snapshot.submitted_tasks, 1);
            assert_eq!(snapshot.cancelled_tasks, 1);
            assert_eq!(snapshot.queued_tasks, 0);
            assert_eq!(snapshot.cancelling_tasks, 0);
            assert!(accounting.is_idle());
        });
        let accepted = accepted.load(StdOrdering::Relaxed);
        let rejected = rejected.load(StdOrdering::Relaxed);
        assert!(
            accepted > 0,
            "model must explore successful reservation after cancellation"
        );
        assert!(
            rejected > 0,
            "model must explore rejected reservation during cancellation"
        );
        eprintln!("reservation witnesses: accepted={accepted}, rejected={rejected}");
    }
}
