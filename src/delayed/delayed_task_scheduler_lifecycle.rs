/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Lifecycle state for delayed task schedulers.

/// Lifecycle state for a delayed task scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelayedTaskSchedulerLifecycle {
    /// New delayed tasks are accepted.
    Running,

    /// New delayed tasks are rejected and accepted tasks are drained.
    Shutdown,

    /// New delayed tasks are rejected and queued tasks are cancelled.
    Stopping,
}

impl DelayedTaskSchedulerLifecycle {
    /// Returns whether this lifecycle accepts new delayed tasks.
    ///
    /// # Returns
    ///
    /// `true` only while the scheduler is running.
    pub const fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}
