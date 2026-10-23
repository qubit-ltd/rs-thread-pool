// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================

#[cfg(not(loom))]
use std::sync::Arc;
#[cfg(not(loom))]
use std::sync::Barrier;
#[cfg(not(loom))]
use std::thread;

#[cfg(all(loom, feature = "loom-model"))]
use loom::model;
#[cfg(all(loom, feature = "loom-model"))]
use loom::sync::Arc as LoomArc;
#[cfg(all(loom, feature = "loom-model"))]
use loom::thread as loom_thread;

use crate::internal::AdmissionGate;

#[cfg(not(loom))]
#[test]
fn test_closes_after_existing_submitter_leaves() {
    let gate = AdmissionGate::new();
    assert!(gate.try_enter());
    gate.close();
    assert!(!gate.try_enter());
    assert!(gate.leave());
    assert_eq!(gate.inflight_count(), 0);
}

#[cfg(not(loom))]
#[test]
fn test_concurrent_close_never_allows_entry_after_close() {
    let gate = Arc::new(AdmissionGate::new());
    let barrier = Arc::new(Barrier::new(2));
    let entered = Arc::clone(&gate);
    let worker_barrier = Arc::clone(&barrier);
    let worker = thread::spawn(move || {
        worker_barrier.wait();
        let mut count = 0;
        for _ in 0..10_000 {
            if entered.try_enter() {
                count += 1;
                let _ = entered.leave();
            }
        }
        count
    });
    gate.close();
    barrier.wait();
    assert_eq!(worker.join().expect("admission worker should finish"), 0);
    assert_eq!(gate.inflight_count(), 0);
}

/// Closing preserves existing entrants and rejects all later entries.
#[cfg(all(loom, feature = "loom-model"))]
#[test]
fn test_loom_close_rejects_later_entry() {
    model(|| {
        let gate = LoomArc::new(AdmissionGate::new());
        assert!(gate.try_enter());
        let closer_gate = LoomArc::clone(&gate);
        loom_thread::spawn(move || closer_gate.close())
            .join()
            .expect("closer should finish");
        assert!(!gate.is_open());
        assert!(!gate.try_enter());
        assert!(gate.leave());
        assert_eq!(gate.inflight_count(), 0);
    });
}

/// A racing entry is either rejected or retained until its matching leave.
#[cfg(all(loom, feature = "loom-model"))]
#[test]
fn test_loom_close_races_with_entry() {
    model(|| {
        let gate = LoomArc::new(AdmissionGate::new());
        let entering_gate = LoomArc::clone(&gate);
        let entrant = loom_thread::spawn(move || entering_gate.try_enter());
        gate.close();
        let entered = entrant.join().expect("entrant should finish");
        assert!(!gate.is_open());
        assert!(!gate.try_enter());
        assert_eq!(gate.inflight_count(), usize::from(entered));
        if entered {
            assert!(gate.leave());
        }
        assert_eq!(gate.inflight_count(), 0);
    });
}
