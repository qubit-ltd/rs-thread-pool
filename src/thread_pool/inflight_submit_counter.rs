/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{
    submission_admission::SubmissionAdmission, submit_enter_outcome::SubmitEnterOutcome,
};

/// Atomic in-flight submit counter.
///
/// This wrapper encapsulates the admission race pattern:
///
/// 1. Check admission before increment.
/// 2. Increment in-flight count.
/// 3. Re-check admission and roll back when it closed concurrently.
pub(super) struct InflightSubmitCounter {
    /// Number of submit calls currently inside admission path.
    count: AtomicUsize,
}

impl InflightSubmitCounter {
    /// Creates an empty in-flight counter.
    ///
    /// # Returns
    ///
    /// Counter initialized to zero.
    pub(super) fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
        }
    }

    /// Returns current in-flight submit count.
    ///
    /// # Returns
    ///
    /// Number of submit calls currently in progress.
    pub(super) fn load(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    /// Attempts to enter one submit operation.
    ///
    /// # Parameters
    ///
    /// * `admission` - Admission gate used to reject late submit attempts.
    ///
    /// # Returns
    ///
    /// [`SubmitEnterOutcome::Entered`] on success, otherwise
    /// [`SubmitEnterOutcome::Rejected`].
    pub(super) fn try_enter(&self, admission: &SubmissionAdmission) -> SubmitEnterOutcome {
        if !admission.is_open() {
            return SubmitEnterOutcome::Rejected {
                became_zero_after_rollback: false,
            };
        }
        self.count.fetch_add(1, Ordering::AcqRel);
        if admission.is_open() {
            return SubmitEnterOutcome::Entered;
        }

        // Admission closed between the first check and increment. Roll back to
        // keep in-flight accounting consistent for shutdown waiters.
        let previous = self.count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(
            previous > 0,
            "thread pool inflight submit counter underflow"
        );
        SubmitEnterOutcome::Rejected {
            became_zero_after_rollback: previous == 1,
        }
    }

    /// Leaves one in-flight submit operation.
    ///
    /// # Returns
    ///
    /// `true` when this call decremented the counter to zero.
    pub(super) fn leave(&self) -> bool {
        let previous = self.count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(
            previous > 0,
            "thread pool inflight submit counter underflow"
        );
        previous == 1
    }
}
