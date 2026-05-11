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
    sync::{
        Arc,
        atomic::{
            AtomicU8,
            Ordering,
        },
    },
    thread,
    time::{
        Duration,
        Instant,
    },
};

use qubit_executor::service::{
    ExecutorServiceLifecycle,
    StopReport,
    SubmissionError,
};

use super::delayed_task_handle::DelayedTaskHandle;
use super::delayed_task_scheduler_inner::DelayedTaskSchedulerInner;
use super::delayed_task_scheduler_worker::DelayedTaskSchedulerWorker;
use super::delayed_task_state::TASK_PENDING;
use super::scheduled_task::ScheduledTask;
use crate::ExecutorServiceBuilderError;

/// Single-threaded scheduler for cancellable delayed tasks.
///
/// The scheduler only owns delay timing. Scheduled closures should stay small;
/// submit longer work to an executor service from the closure.
pub struct DelayedTaskScheduler {
    /// Shared scheduler state.
    inner: Arc<DelayedTaskSchedulerInner>,
}

impl DelayedTaskScheduler {
    /// Starts a new delayed task scheduler.
    ///
    /// # Parameters
    ///
    /// * `thread_name` - Name for the scheduler thread.
    ///
    /// # Returns
    ///
    /// A started delayed task scheduler.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorServiceBuilderError::SpawnWorker`] if the scheduler thread
    /// cannot be created.
    pub fn new(thread_name: &str) -> Result<Self, ExecutorServiceBuilderError> {
        Self::with_stack_size(thread_name, None)
    }

    /// Starts a new delayed task scheduler with an optional thread stack size.
    ///
    /// # Parameters
    ///
    /// * `thread_name` - Name for the scheduler thread.
    /// * `stack_size` - Optional stack size for the scheduler thread.
    ///
    /// # Returns
    ///
    /// A started delayed task scheduler.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorServiceBuilderError::SpawnWorker`] if the scheduler thread
    /// cannot be created.
    pub fn with_stack_size(
        thread_name: &str,
        stack_size: Option<usize>,
    ) -> Result<Self, ExecutorServiceBuilderError> {
        let inner = Arc::new(DelayedTaskSchedulerInner::new());
        let worker_inner = Arc::clone(&inner);
        let mut builder = thread::Builder::new().name(thread_name.to_string());
        if let Some(stack_size) = stack_size {
            builder = builder.stack_size(stack_size);
        }
        let worker = builder.spawn(move || DelayedTaskSchedulerWorker::run(worker_inner));
        if let Err(source) = worker {
            return Err(ExecutorServiceBuilderError::SpawnWorker {
                index: Some(0),
                source,
            });
        }
        Ok(Self { inner })
    }

    /// Schedules a task to run after the given delay.
    ///
    /// # Parameters
    ///
    /// * `delay` - Minimum delay before the task becomes runnable.
    /// * `task` - Action to run on the scheduler thread after the delay.
    ///
    /// # Returns
    ///
    /// A handle that can cancel the task before it starts.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] after shutdown starts.
    pub fn schedule<F>(
        &self,
        delay: Duration,
        task: F,
    ) -> Result<DelayedTaskHandle, SubmissionError>
    where
        F: FnOnce() + Send + 'static,
    {
        let task_state = Arc::new(AtomicU8::new(TASK_PENDING));
        let inner_for_cancel = Arc::downgrade(&self.inner);
        let handle = DelayedTaskHandle::new(
            Arc::clone(&task_state),
            Arc::new(move || {
                if let Some(inner) = inner_for_cancel.upgrade() {
                    inner.finish_queued_cancellation();
                }
            }),
        );
        let deadline = Instant::now() + delay;
        let mut state = self.inner.state.lock();
        if state.lifecycle != ExecutorServiceLifecycle::Running {
            return Err(SubmissionError::Shutdown);
        }
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.wrapping_add(1);
        state.tasks.push(ScheduledTask::new(
            deadline,
            sequence,
            task_state,
            Box::new(task),
        ));
        self.inner.queued_task_count.fetch_add(1, Ordering::AcqRel);
        self.inner.state.notify_all();
        Ok(handle)
    }

    /// Requests graceful shutdown.
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }

    /// Requests immediate shutdown and cancels pending delayed tasks.
    ///
    /// # Returns
    ///
    /// Count-based shutdown report.
    pub fn stop(&self) -> StopReport {
        self.inner.stop()
    }

    /// Returns the current lifecycle state.
    ///
    /// # Returns
    ///
    /// [`ExecutorServiceLifecycle::Terminated`] after the scheduler thread has
    /// exited, otherwise the stored lifecycle state.
    pub fn lifecycle(&self) -> ExecutorServiceLifecycle {
        self.inner.lifecycle()
    }

    /// Returns whether this scheduler still accepts delayed tasks.
    ///
    /// # Returns
    ///
    /// `true` only while the lifecycle is [`ExecutorServiceLifecycle::Running`].
    pub fn is_running(&self) -> bool {
        self.lifecycle() == ExecutorServiceLifecycle::Running
    }

    /// Returns whether graceful shutdown is in progress.
    ///
    /// # Returns
    ///
    /// `true` only while the lifecycle is
    /// [`ExecutorServiceLifecycle::ShuttingDown`].
    pub fn is_shutting_down(&self) -> bool {
        self.lifecycle() == ExecutorServiceLifecycle::ShuttingDown
    }

    /// Returns whether abrupt stop is in progress.
    ///
    /// # Returns
    ///
    /// `true` only while the lifecycle is [`ExecutorServiceLifecycle::Stopping`].
    pub fn is_stopping(&self) -> bool {
        self.lifecycle() == ExecutorServiceLifecycle::Stopping
    }

    /// Returns whether shutdown has started.
    ///
    /// # Returns
    ///
    /// `true` if this scheduler rejects new tasks.
    pub fn is_not_running(&self) -> bool {
        self.inner.is_not_running()
    }

    /// Returns whether the scheduler thread has exited.
    ///
    /// # Returns
    ///
    /// `true` after shutdown and termination.
    pub fn is_terminated(&self) -> bool {
        self.inner.is_terminated()
    }

    /// Returns the number of pending delayed tasks.
    ///
    /// # Returns
    ///
    /// Number of accepted delayed tasks that have not started or been
    /// cancelled.
    pub fn queued_count(&self) -> usize {
        self.inner.queued_count()
    }

    /// Blocks until the scheduler thread has terminated.
    pub fn wait_termination(&self) {
        self.inner.wait_for_termination();
    }
}

impl Drop for DelayedTaskScheduler {
    fn drop(&mut self) {
        self.inner.shutdown();
    }
}
