/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::sync::atomic::{AtomicBool, Ordering};

/// CAS gate controlling whether submit calls may enter admission.
pub(super) struct SubmissionAdmission {
    /// `true` while new submissions may attempt admission.
    open: AtomicBool,
}

impl SubmissionAdmission {
    /// Creates an admission gate in open state.
    ///
    /// # Returns
    ///
    /// Admission gate that accepts new submissions.
    pub(super) fn new_open() -> Self {
        Self {
            open: AtomicBool::new(true),
        }
    }

    /// Returns whether admission is currently open.
    ///
    /// # Returns
    ///
    /// `true` when submit calls may still enter admission.
    pub(super) fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    /// Closes admission using a CAS transition.
    ///
    /// # Returns
    ///
    /// `true` when this call closed the gate, or `false` when it was already
    /// closed.
    pub(super) fn close(&self) -> bool {
        self.open
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}
