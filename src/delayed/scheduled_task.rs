/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::{
    cmp::Ordering as CompareOrdering,
    sync::{
        Arc,
        atomic::AtomicU8,
    },
    time::Instant,
};

/// Task stored in the delayed scheduler heap.
pub struct ScheduledTask {
    /// Time at which this task becomes runnable.
    pub deadline: Instant,
    /// Insertion order used to make equal deadlines deterministic.
    pub sequence: usize,
    /// Shared task state observed by cancellation handles.
    pub state: Arc<AtomicU8>,
    /// Scheduled action.
    pub task: Option<Box<dyn FnOnce() + Send + 'static>>,
}

impl ScheduledTask {
    /// Creates a heap entry for a delayed task.
    ///
    /// # Parameters
    ///
    /// * `deadline` - Instant when the task becomes runnable.
    /// * `sequence` - Stable insertion sequence.
    /// * `state` - Shared task lifecycle state.
    /// * `task` - Action to run when the deadline is reached.
    ///
    /// # Returns
    ///
    /// A scheduled task heap entry.
    pub fn new(
        deadline: Instant,
        sequence: usize,
        state: Arc<AtomicU8>,
        task: Box<dyn FnOnce() + Send + 'static>,
    ) -> Self {
        Self {
            deadline,
            sequence,
            state,
            task: Some(task),
        }
    }
}

impl Eq for ScheduledTask {}

impl PartialEq for ScheduledTask {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.sequence == other.sequence
    }
}

impl Ord for ScheduledTask {
    fn cmp(&self, other: &Self) -> CompareOrdering {
        other
            .deadline
            .cmp(&self.deadline)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for ScheduledTask {
    fn partial_cmp(&self, other: &Self) -> Option<CompareOrdering> {
        Some(self.cmp(other))
    }
}
