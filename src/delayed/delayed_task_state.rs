/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::sync::atomic::{AtomicU8, Ordering};

/// Atomic task-state transitions for delayed task scheduling.
pub struct DelayedTaskState;

impl DelayedTaskState {
    /// Task has been accepted but has not started.
    pub const PENDING: u8 = 0;
    /// Task has started and can no longer be cancelled.
    pub const STARTED: u8 = 1;
    /// Task was cancelled before it started.
    pub const CANCELLED: u8 = 2;

    /// Attempts to mark a task state as cancelled.
    pub fn cancel(task_state: &AtomicU8) -> bool {
        task_state
            .compare_exchange(
                Self::PENDING,
                Self::CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Attempts to mark a task state as started.
    pub fn start(task_state: &AtomicU8) -> bool {
        task_state
            .compare_exchange(
                Self::PENDING,
                Self::STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Returns whether a task state has been cancelled.
    pub fn is_cancelled(task_state: &AtomicU8) -> bool {
        task_state.load(Ordering::Acquire) == Self::CANCELLED
    }
}

pub const TASK_PENDING: u8 = DelayedTaskState::PENDING;
pub const TASK_CANCELLED: u8 = DelayedTaskState::CANCELLED;

/// Attempts to mark a task state as cancelled.
pub fn cancel_task_state(task_state: &AtomicU8) -> bool {
    DelayedTaskState::cancel(task_state)
}

/// Attempts to mark a task state as started.
pub fn start_task_state(task_state: &AtomicU8) -> bool {
    DelayedTaskState::start(task_state)
}

/// Returns whether a task state has been cancelled.
pub fn is_task_cancelled(task_state: &AtomicU8) -> bool {
    DelayedTaskState::is_cancelled(task_state)
}
