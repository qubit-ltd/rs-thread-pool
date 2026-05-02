/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
/// Result of trying to enter submit in-flight accounting.
pub(super) enum SubmitEnterOutcome {
    /// Submit call entered successfully and must later call `leave`.
    Entered,

    /// Submit call was rejected because admission closed.
    ///
    /// `became_zero_after_rollback` indicates whether rollback decremented the
    /// in-flight counter to zero; shutdown waiters should be notified in that
    /// case.
    Rejected { became_zero_after_rollback: bool },
}
