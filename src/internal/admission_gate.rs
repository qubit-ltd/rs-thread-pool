// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

const CLOSED: usize = 1usize << (usize::BITS - 1);
const COUNT_MASK: usize = CLOSED - 1;

/// Linearizable admission state shared by an executor and its submitters.
pub(crate) struct AdmissionGate {
    state: AtomicUsize,
}

impl AdmissionGate {
    /// Creates an open admission gate with no submitters.
    pub(crate) const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    /// Enters admission unless the gate has been closed.
    pub(crate) fn try_enter(&self) -> bool {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            if current & CLOSED != 0 {
                return false;
            }
            assert!(current & COUNT_MASK < COUNT_MASK, "admission count overflow");
            match self
                .state
                .compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    /// Closes the gate. Existing entrants are allowed to leave normally.
    pub(crate) fn close(&self) {
        self.state.fetch_or(CLOSED, Ordering::AcqRel);
    }

    /// Leaves admission and reports whether this was the last entrant.
    pub(crate) fn leave(&self) -> bool {
        let previous = self.state.fetch_sub(1, Ordering::AcqRel);
        assert!(previous & COUNT_MASK > 0, "admission count underflow");
        previous & COUNT_MASK == 1
    }

    /// Returns the number of submitters currently inside admission.
    pub(crate) fn inflight_count(&self) -> usize {
        self.state.load(Ordering::Acquire) & COUNT_MASK
    }

    /// Returns whether new submissions may enter.
    pub(crate) fn is_open(&self) -> bool {
        self.state.load(Ordering::Acquire) & CLOSED == 0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::thread;

    use super::AdmissionGate;

    #[test]
    fn closes_after_existing_submitter_leaves() {
        let gate = AdmissionGate::new();
        assert!(gate.try_enter());
        gate.close();
        assert!(!gate.try_enter());
        assert!(gate.leave());
        assert_eq!(gate.inflight_count(), 0);
    }

    #[test]
    fn concurrent_close_never_allows_entry_after_close() {
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
                    entered.leave();
                }
            }
            count
        });
        gate.close();
        barrier.wait();
        assert_eq!(worker.join().expect("admission worker should finish"), 0);
        assert_eq!(gate.inflight_count(), 0);
    }
}
