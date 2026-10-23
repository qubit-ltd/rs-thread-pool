// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================

use super::sync::AtomicUsize;
use super::sync::Ordering;

const CLOSED: usize = 1usize << (usize::BITS - 1);
const COUNT_MASK: usize = CLOSED - 1;

/// Linearizable admission state shared by an executor and its submitters.
///
/// The high bit closes admission, while the remaining bits count active
/// submitters. Closing prevents new entrants but preserves the count until
/// each admitted submitter leaves.
pub(crate) struct AdmissionGate {
    /// Packs the closed flag and in-flight submitter count into one atomic
    /// word.
    state: AtomicUsize,
}

impl AdmissionGate {
    /// Creates an open admission gate with no submitters.
    #[must_use]
    #[inline]
    pub(crate) fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    /// Enters admission unless the gate has been closed.
    ///
    /// Returns `true` after incrementing the active submitter count, or `false`
    /// if closure was observed. Panics if the active count would overflow its
    /// available bits.
    #[must_use]
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
    ///
    /// This operation is idempotent and prevents subsequent successful calls
    /// to `try_enter`.
    #[inline]
    pub(crate) fn close(&self) {
        self.state.fetch_or(CLOSED, Ordering::AcqRel);
    }

    /// Leaves admission and reports whether this was the last entrant.
    ///
    /// The caller must have previously entered. Returns `true` when this call
    /// reduces the active submitter count to zero. Panics if the count is
    /// already zero.
    #[must_use]
    #[inline]
    pub(crate) fn leave(&self) -> bool {
        let previous = self.state.fetch_sub(1, Ordering::AcqRel);
        assert!(previous & COUNT_MASK > 0, "admission count underflow");
        previous & COUNT_MASK == 1
    }

    /// Returns the number of submitters currently inside admission.
    ///
    /// The result excludes the closed bit and may change immediately after
    /// this atomic snapshot.
    #[must_use]
    #[inline]
    pub(crate) fn inflight_count(&self) -> usize {
        self.state.load(Ordering::Acquire) & COUNT_MASK
    }

    /// Returns whether new submissions may enter.
    ///
    /// The result is an atomic snapshot; the gate may close immediately after
    /// this method returns.
    #[must_use]
    #[inline]
    pub(crate) fn is_open(&self) -> bool {
        self.state.load(Ordering::Acquire) & CLOSED == 0
    }
}
