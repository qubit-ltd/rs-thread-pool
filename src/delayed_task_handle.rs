/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Cancellation handle for delayed task scheduling.

use std::sync::{
    Arc,
    atomic::{
        AtomicU8,
        Ordering,
    },
};

pub(crate) const TASK_PENDING: u8 = 0;
pub(crate) const TASK_STARTED: u8 = 1;
pub(crate) const TASK_CANCELLED: u8 = 2;

/// Handle that can cancel a delayed task before it starts.
#[derive(Clone)]
pub struct DelayedTaskHandle {
    /// Shared lifecycle state for the scheduled task.
    state: Arc<AtomicU8>,
    /// Callback invoked after this handle changes the task to cancelled.
    on_cancelled: Arc<dyn Fn() + Send + Sync + 'static>,
}

impl DelayedTaskHandle {
    /// Creates a delayed task handle.
    pub(crate) fn new(
        state: Arc<AtomicU8>,
        on_cancelled: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        Self {
            state,
            on_cancelled,
        }
    }

    /// Cancels the delayed task if it has not started.
    ///
    /// # Returns
    ///
    /// `true` if this call cancelled the task.
    pub fn cancel(&self) -> bool {
        let cancelled = self
            .state
            .compare_exchange(
                TASK_PENDING,
                TASK_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if cancelled {
            (self.on_cancelled)();
        }
        cancelled
    }

    /// Returns whether this delayed task has been cancelled.
    ///
    /// # Returns
    ///
    /// `true` when the task was cancelled before it started.
    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == TASK_CANCELLED
    }
}
