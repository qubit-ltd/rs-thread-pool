/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};

use qubit_executor::service::{ExecutorServiceLifecycle, StopReport};

use super::delayed_task_scheduler_state::DelayedTaskSchedulerState;
use super::delayed_task_state::{cancel_task_state, start_task_state};

/// Shared delayed scheduler state.
pub struct DelayedTaskSchedulerInner {
    /// Mutable lifecycle and heap state.
    pub state: Mutex<DelayedTaskSchedulerState>,
    /// Wait set for scheduler state transitions and deadline changes.
    pub condition: Condvar,
    /// Number of tasks still pending in the delay heap.
    pub queued_task_count: AtomicUsize,
    /// Number of tasks currently executing on the scheduler thread.
    pub running_task_count: AtomicUsize,
    /// Number of tasks that ran to completion.
    pub completed_task_count: AtomicUsize,
    /// Number of delayed tasks cancelled before execution.
    pub cancelled_task_count: AtomicUsize,
}

impl DelayedTaskSchedulerInner {
    /// Creates an empty delayed scheduler.
    ///
    /// # Returns
    ///
    /// Shared scheduler state before its worker thread starts.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(DelayedTaskSchedulerState::new()),
            condition: Condvar::new(),
            queued_task_count: AtomicUsize::new(0),
            running_task_count: AtomicUsize::new(0),
            completed_task_count: AtomicUsize::new(0),
            cancelled_task_count: AtomicUsize::new(0),
        }
    }

    /// Returns the queued delayed task count.
    ///
    /// # Returns
    ///
    /// Number of tasks that have not started or been cancelled.
    #[inline]
    pub fn queued_count(&self) -> usize {
        self.queued_task_count.load(Ordering::Acquire)
    }

    /// Returns the currently running task count.
    ///
    /// # Returns
    ///
    /// `1` when the scheduler thread is running a task, otherwise `0`.
    #[inline]
    pub fn running_count(&self) -> usize {
        self.running_task_count.load(Ordering::Acquire)
    }

    /// Records a pending task cancellation.
    pub fn finish_queued_cancellation(&self) {
        let previous = self.queued_task_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "delayed scheduler queued counter underflow");
        self.cancelled_task_count.fetch_add(1, Ordering::AcqRel);
        self.condition.notify_all();
    }

    /// Attempts to cancel a task state before it starts.
    ///
    /// # Parameters
    ///
    /// * `task_state` - Shared task lifecycle state.
    ///
    /// # Returns
    ///
    /// `true` if this call cancelled the task.
    pub fn cancel_task_state(&self, task_state: &AtomicU8) -> bool {
        if cancel_task_state(task_state) {
            self.finish_queued_cancellation();
            true
        } else {
            false
        }
    }

    /// Marks a task as started if it has not been cancelled.
    ///
    /// # Parameters
    ///
    /// * `task_state` - Shared task lifecycle state.
    ///
    /// # Returns
    ///
    /// `true` if the task may execute.
    pub fn start_task_state(&self, task_state: &AtomicU8) -> bool {
        if start_task_state(task_state) {
            let previous = self.queued_task_count.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "delayed scheduler queued counter underflow");
            true
        } else {
            false
        }
    }

    /// Requests graceful shutdown.
    pub fn shutdown(&self) {
        let mut state = self.state.lock().expect("scheduler state should lock");
        if state.lifecycle == ExecutorServiceLifecycle::Running {
            state.lifecycle = ExecutorServiceLifecycle::ShuttingDown;
        }
        self.condition.notify_all();
    }

    /// Requests immediate shutdown and cancels all queued delayed tasks.
    ///
    /// # Returns
    ///
    /// Count-based shutdown report.
    pub fn stop(&self) -> StopReport {
        let mut state = self.state.lock().expect("scheduler state should lock");
        state.lifecycle = ExecutorServiceLifecycle::Stopping;
        let mut cancelled = 0;
        while let Some(task) = state.tasks.pop() {
            if self.cancel_task_state(&task.state) {
                cancelled += 1;
            }
        }
        let running = self.running_count();
        self.condition.notify_all();
        StopReport::new(cancelled, running, cancelled)
    }

    /// Returns whether shutdown has started.
    ///
    /// # Returns
    ///
    /// `true` if new delayed tasks are rejected.
    pub fn is_not_running(&self) -> bool {
        let state = self.state.lock().expect("scheduler state should lock");
        state.lifecycle != ExecutorServiceLifecycle::Running
    }

    /// Returns the current lifecycle state.
    ///
    /// # Returns
    ///
    /// [`ExecutorServiceLifecycle::Terminated`] after the worker has exited,
    /// otherwise the stored lifecycle state.
    pub fn lifecycle(&self) -> ExecutorServiceLifecycle {
        let state = self.state.lock().expect("scheduler state should lock");
        if state.terminated {
            ExecutorServiceLifecycle::Terminated
        } else {
            state.lifecycle
        }
    }

    /// Returns whether the scheduler thread has exited.
    ///
    /// # Returns
    ///
    /// `true` after shutdown and scheduler termination.
    pub fn is_terminated(&self) -> bool {
        let state = self.state.lock().expect("scheduler state should lock");
        state.terminated
    }

    /// Waits until the scheduler thread exits.
    pub fn wait_for_termination(&self) {
        let mut state = self.state.lock().expect("scheduler state should lock");
        while !state.terminated {
            state = self
                .condition
                .wait(state)
                .expect("scheduler state wait should not poison");
        }
    }

    /// Marks the scheduler thread as terminated.
    pub fn terminate(&self, state: &mut DelayedTaskSchedulerState) {
        state.terminated = true;
        self.condition.notify_all();
    }
}

impl Default for DelayedTaskSchedulerInner {
    fn default() -> Self {
        Self::new()
    }
}
