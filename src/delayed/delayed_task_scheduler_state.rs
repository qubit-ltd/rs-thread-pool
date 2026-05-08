/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::collections::BinaryHeap;

use super::delayed_task_scheduler_lifecycle::DelayedTaskSchedulerLifecycle;
use super::scheduled_task::ScheduledTask;

/// Mutable scheduler state protected by the scheduler mutex.
pub struct DelayedTaskSchedulerState {
    /// Current lifecycle state.
    pub lifecycle: DelayedTaskSchedulerLifecycle,
    /// Deadline-ordered task heap.
    pub tasks: BinaryHeap<ScheduledTask>,
    /// Sequence used to keep stable order for identical deadlines.
    pub next_sequence: usize,
    /// Whether the scheduler thread has exited.
    pub terminated: bool,
}

impl DelayedTaskSchedulerState {
    /// Creates an empty running scheduler state.
    ///
    /// # Returns
    ///
    /// A running state with no queued delayed tasks.
    pub(crate) fn new() -> Self {
        Self {
            lifecycle: DelayedTaskSchedulerLifecycle::Running,
            tasks: BinaryHeap::new(),
            next_sequence: 0,
            terminated: false,
        }
    }
}
