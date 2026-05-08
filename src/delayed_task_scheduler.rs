/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
// qubit-style: allow inline-tests

use std::{
    cmp::Ordering as CompareOrdering,
    collections::BinaryHeap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        Condvar,
        Mutex,
        atomic::{
            AtomicU8,
            AtomicUsize,
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
    RejectedExecution,
    ShutdownReport,
};

use super::thread_pool::ThreadPoolBuildError;
use crate::delayed_task_handle::DelayedTaskHandle;
use crate::delayed_task_handle::{
    TASK_CANCELLED,
    TASK_PENDING,
    TASK_STARTED,
};

/// Lifecycle state for a delayed task scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelayedTaskSchedulerLifecycle {
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
    const fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

/// Mutable scheduler state protected by the scheduler mutex.
struct DelayedTaskSchedulerState {
    /// Current lifecycle state.
    lifecycle: DelayedTaskSchedulerLifecycle,
    /// Deadline-ordered task heap.
    tasks: BinaryHeap<ScheduledTask>,
    /// Sequence used to keep stable order for identical deadlines.
    next_sequence: usize,
    /// Whether the scheduler thread has exited.
    terminated: bool,
}

impl DelayedTaskSchedulerState {
    /// Creates an empty running scheduler state.
    ///
    /// # Returns
    ///
    /// A running state with no queued delayed tasks.
    fn new() -> Self {
        Self {
            lifecycle: DelayedTaskSchedulerLifecycle::Running,
            tasks: BinaryHeap::new(),
            next_sequence: 0,
            terminated: false,
        }
    }
}

/// Shared delayed scheduler state.
struct DelayedTaskSchedulerInner {
    /// Mutable lifecycle and heap state.
    state: Mutex<DelayedTaskSchedulerState>,
    /// Wait set for scheduler state transitions and deadline changes.
    condition: Condvar,
    /// Fast-path admission flag.
    accepting: AtomicU8,
    /// Number of tasks still pending in the delay heap.
    queued_task_count: AtomicUsize,
    /// Number of tasks currently executing on the scheduler thread.
    running_task_count: AtomicUsize,
    /// Number of tasks that ran to completion.
    completed_task_count: AtomicUsize,
    /// Number of delayed tasks cancelled before execution.
    cancelled_task_count: AtomicUsize,
}

impl DelayedTaskSchedulerInner {
    /// Creates an empty delayed scheduler.
    ///
    /// # Returns
    ///
    /// Shared scheduler state before its worker thread starts.
    fn new() -> Self {
        Self {
            state: Mutex::new(DelayedTaskSchedulerState::new()),
            condition: Condvar::new(),
            accepting: AtomicU8::new(1),
            queued_task_count: AtomicUsize::new(0),
            running_task_count: AtomicUsize::new(0),
            completed_task_count: AtomicUsize::new(0),
            cancelled_task_count: AtomicUsize::new(0),
        }
    }

    /// Returns whether submissions are still accepted.
    ///
    /// # Returns
    ///
    /// `true` while the scheduler is running.
    #[inline]
    fn accepts_tasks(&self) -> bool {
        self.accepting.load(Ordering::Acquire) == 1
    }

    /// Returns the queued delayed task count.
    ///
    /// # Returns
    ///
    /// Number of tasks that have not started or been cancelled.
    #[inline]
    fn queued_count(&self) -> usize {
        self.queued_task_count.load(Ordering::Acquire)
    }

    /// Returns the currently running task count.
    ///
    /// # Returns
    ///
    /// `1` when the scheduler thread is running a task, otherwise `0`.
    #[inline]
    fn running_count(&self) -> usize {
        self.running_task_count.load(Ordering::Acquire)
    }

    /// Records a pending task cancellation.
    fn finish_queued_cancellation(&self) {
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
    fn cancel_task_state(&self, task_state: &AtomicU8) -> bool {
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
    fn start_task_state(&self, task_state: &AtomicU8) -> bool {
        if start_task_state(task_state) {
            let previous = self.queued_task_count.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "delayed scheduler queued counter underflow");
            true
        } else {
            false
        }
    }

    /// Requests graceful shutdown.
    fn shutdown(&self) {
        self.accepting.store(0, Ordering::Release);
        let mut state = self.state.lock().expect("scheduler state should lock");
        if state.lifecycle.is_running() {
            state.lifecycle = DelayedTaskSchedulerLifecycle::Shutdown;
        }
        self.condition.notify_all();
    }

    /// Requests immediate shutdown and cancels all queued delayed tasks.
    ///
    /// # Returns
    ///
    /// Count-based shutdown report.
    fn shutdown_now(&self) -> ShutdownReport {
        self.accepting.store(0, Ordering::Release);
        let mut state = self.state.lock().expect("scheduler state should lock");
        state.lifecycle = DelayedTaskSchedulerLifecycle::Stopping;
        let mut cancelled = 0;
        while let Some(task) = state.tasks.pop() {
            if self.cancel_task_state(&task.state) {
                cancelled += 1;
            }
        }
        let running = self.running_count();
        self.condition.notify_all();
        ShutdownReport::new(cancelled, running, cancelled)
    }

    /// Returns whether shutdown has started.
    ///
    /// # Returns
    ///
    /// `true` if new delayed tasks are rejected.
    fn is_shutdown(&self) -> bool {
        let state = self.state.lock().expect("scheduler state should lock");
        !state.lifecycle.is_running()
    }

    /// Returns whether the scheduler thread has exited.
    ///
    /// # Returns
    ///
    /// `true` after shutdown and scheduler termination.
    fn is_terminated(&self) -> bool {
        let state = self.state.lock().expect("scheduler state should lock");
        state.terminated
    }

    /// Waits until the scheduler thread exits.
    fn wait_for_termination(&self) {
        let mut state = self.state.lock().expect("scheduler state should lock");
        while !state.terminated {
            state = self
                .condition
                .wait(state)
                .expect("scheduler state wait should not poison");
        }
    }

    /// Marks the scheduler thread as terminated.
    fn terminate(&self, state: &mut DelayedTaskSchedulerState) {
        state.terminated = true;
        self.condition.notify_all();
    }
}

/// Task stored in the delayed scheduler heap.
struct ScheduledTask {
    /// Time at which this task becomes runnable.
    deadline: Instant,
    /// Insertion order used to make equal deadlines deterministic.
    sequence: usize,
    /// Shared task state observed by cancellation handles.
    state: Arc<AtomicU8>,
    /// Scheduled action.
    task: Option<Box<dyn FnOnce() + Send + 'static>>,
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
    fn new(
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
    /// Returns [`ThreadPoolBuildError::SpawnWorker`] if the scheduler thread
    /// cannot be created.
    pub fn new(thread_name: &str) -> Result<Self, ThreadPoolBuildError> {
        let inner = Arc::new(DelayedTaskSchedulerInner::new());
        let worker_inner = Arc::clone(&inner);
        let worker = thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || run_delayed_scheduler(worker_inner));
        if let Err(source) = worker {
            return Err(ThreadPoolBuildError::SpawnWorker { index: 0, source });
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
    /// Returns [`RejectedExecution::Shutdown`] after shutdown starts.
    pub fn schedule<F>(
        &self,
        delay: Duration,
        task: F,
    ) -> Result<DelayedTaskHandle, RejectedExecution>
    where
        F: FnOnce() + Send + 'static,
    {
        if !self.inner.accepts_tasks() {
            return Err(RejectedExecution::Shutdown);
        }
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
        let mut state = self
            .inner
            .state
            .lock()
            .expect("scheduler state should lock");
        if !state.lifecycle.is_running() {
            return Err(RejectedExecution::Shutdown);
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
        self.inner.condition.notify_all();
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
    pub fn shutdown_now(&self) -> ShutdownReport {
        self.inner.shutdown_now()
    }

    /// Returns whether shutdown has started.
    ///
    /// # Returns
    ///
    /// `true` if this scheduler rejects new tasks.
    pub fn is_shutdown(&self) -> bool {
        self.inner.is_shutdown()
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

    /// Waits until the scheduler thread has terminated.
    ///
    /// # Returns
    ///
    /// A future that blocks the polling thread until termination.
    pub fn await_termination(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.inner.wait_for_termination();
        })
    }
}

impl Drop for DelayedTaskScheduler {
    fn drop(&mut self) {
        self.inner.shutdown();
    }
}

/// Runs the delayed task scheduler loop.
///
/// # Parameters
///
/// * `inner` - Shared scheduler state.
fn run_delayed_scheduler(inner: Arc<DelayedTaskSchedulerInner>) {
    loop {
        let task = {
            let mut state = inner.state.lock().expect("scheduler state should lock");
            loop {
                prune_cancelled_front(&mut state);
                if state.lifecycle == DelayedTaskSchedulerLifecycle::Stopping {
                    inner.terminate(&mut state);
                    return;
                }
                if state.tasks.is_empty() && !state.lifecycle.is_running() {
                    inner.terminate(&mut state);
                    return;
                }
                let Some(next_deadline) = state.tasks.peek().map(|task| task.deadline) else {
                    state = inner
                        .condition
                        .wait(state)
                        .expect("scheduler state wait should not poison");
                    continue;
                };
                let now = Instant::now();
                if next_deadline > now {
                    let timeout = next_deadline.saturating_duration_since(now);
                    let (next_state, _) = inner
                        .condition
                        .wait_timeout(state, timeout)
                        .expect("scheduler state wait should not poison");
                    state = next_state;
                    continue;
                }
                break state.tasks.pop();
            }
        };
        if let Some(mut task) = task {
            if !inner.start_task_state(&task.state) {
                continue;
            }
            let Some(action) = task.task.take() else {
                continue;
            };
            inner.running_task_count.fetch_add(1, Ordering::AcqRel);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));
            inner.running_task_count.fetch_sub(1, Ordering::AcqRel);
            inner.completed_task_count.fetch_add(1, Ordering::AcqRel);
            inner.condition.notify_all();
        }
    }
}

/// Removes already-cancelled tasks from the front of the deadline heap.
///
/// # Parameters
///
/// * `state` - Locked scheduler state.
fn prune_cancelled_front(state: &mut DelayedTaskSchedulerState) {
    while state
        .tasks
        .peek()
        .is_some_and(|task| is_task_cancelled(&task.state))
    {
        state.tasks.pop();
    }
}

/// Attempts to mark a task state as cancelled.
fn cancel_task_state(task_state: &AtomicU8) -> bool {
    task_state
        .compare_exchange(
            TASK_PENDING,
            TASK_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

/// Attempts to mark a task state as started.
fn start_task_state(task_state: &AtomicU8) -> bool {
    task_state
        .compare_exchange(
            TASK_PENDING,
            TASK_STARTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

/// Returns whether a task state has been cancelled.
fn is_task_cancelled(task_state: &AtomicU8) -> bool {
    task_state.load(Ordering::Acquire) == TASK_CANCELLED
}

#[cfg(test)]
mod tests {
    use std::{
        cmp::Ordering as CompareOrdering,
        sync::{
            Arc,
            atomic::{
                AtomicBool,
                AtomicU8,
                Ordering,
            },
            mpsc,
        },
        thread,
        time::{
            Duration,
            Instant,
        },
    };

    use qubit_executor::service::RejectedExecution;

    use crate::delayed_task_handle::{
        TASK_CANCELLED,
        TASK_PENDING,
        TASK_STARTED,
    };

    use super::{
        DelayedTaskScheduler,
        DelayedTaskSchedulerInner,
        DelayedTaskSchedulerLifecycle,
        DelayedTaskSchedulerState,
        ScheduledTask,
        is_task_cancelled,
        prune_cancelled_front,
        run_delayed_scheduler,
    };

    fn test_noop_task() {}

    fn create_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build")
    }

    fn wait_until<F>(mut condition: F)
    where
        F: FnMut() -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(condition(), "condition should become true within timeout");
    }

    #[test]
    fn test_delayed_scheduler_inner_state_helpers_cover_counters_and_lifecycle() {
        let inner = DelayedTaskSchedulerInner::new();

        test_noop_task();
        assert!(inner.accepts_tasks());
        assert_eq!(inner.queued_count(), 0);
        assert_eq!(inner.running_count(), 0);
        assert!(!inner.is_shutdown());
        assert!(!inner.is_terminated());

        inner.shutdown();
        inner.shutdown();
        assert!(!inner.accepts_tasks());
        assert!(inner.is_shutdown());
        {
            let mut state = inner.state.lock().expect("scheduler state should lock");
            inner.terminate(&mut state);
        }
        inner.wait_for_termination();
        assert!(inner.is_terminated());
    }

    #[test]
    fn test_delayed_scheduler_task_state_helpers_cover_all_transitions() {
        let inner = DelayedTaskSchedulerInner::new();

        let cancelled = AtomicU8::new(TASK_PENDING);
        inner.queued_task_count.store(1, Ordering::Release);
        assert!(inner.cancel_task_state(&cancelled));
        assert!(is_task_cancelled(&cancelled));
        assert!(!inner.cancel_task_state(&cancelled));

        let started = AtomicU8::new(TASK_PENDING);
        inner.queued_task_count.store(1, Ordering::Release);
        assert!(inner.start_task_state(&started));
        assert!(!inner.start_task_state(&started));
        assert!(!inner.cancel_task_state(&started));

        let already_cancelled = AtomicU8::new(TASK_CANCELLED);
        assert!(!inner.start_task_state(&already_cancelled));
    }

    #[test]
    fn test_scheduled_task_ordering_and_cancelled_pruning() {
        let now = Instant::now();
        let first = ScheduledTask::new(
            now + Duration::from_millis(5),
            1,
            Arc::new(AtomicU8::new(TASK_PENDING)),
            Box::new(test_noop_task),
        );
        let same = ScheduledTask::new(
            now + Duration::from_millis(5),
            1,
            Arc::new(AtomicU8::new(TASK_PENDING)),
            Box::new(test_noop_task),
        );
        let later = ScheduledTask::new(
            now + Duration::from_millis(10),
            0,
            Arc::new(AtomicU8::new(TASK_PENDING)),
            Box::new(test_noop_task),
        );
        let same_deadline_later_sequence = ScheduledTask::new(
            now + Duration::from_millis(5),
            2,
            Arc::new(AtomicU8::new(TASK_PENDING)),
            Box::new(test_noop_task),
        );

        assert!(first == same);
        assert_eq!(first.partial_cmp(&later), Some(CompareOrdering::Greater));
        assert_eq!(
            first.cmp(&same_deadline_later_sequence),
            CompareOrdering::Greater,
        );

        let cancelled_state = Arc::new(AtomicU8::new(TASK_CANCELLED));
        let pending_state = Arc::new(AtomicU8::new(TASK_PENDING));
        let mut state = DelayedTaskSchedulerState::new();
        state.tasks.push(ScheduledTask::new(
            now,
            0,
            cancelled_state,
            Box::new(test_noop_task),
        ));
        state.tasks.push(ScheduledTask::new(
            now + Duration::from_millis(1),
            1,
            Arc::clone(&pending_state),
            Box::new(test_noop_task),
        ));

        prune_cancelled_front(&mut state);

        assert_eq!(state.tasks.len(), 1);
        assert!(Arc::ptr_eq(
            &state
                .tasks
                .peek()
                .expect("pending task should remain")
                .state,
            &pending_state,
        ));
    }

    #[test]
    fn test_delayed_scheduler_rejects_when_state_stops_after_admission_check() {
        let scheduler = DelayedTaskScheduler {
            inner: Arc::new(DelayedTaskSchedulerInner::new()),
        };
        {
            let mut state = scheduler
                .inner
                .state
                .lock()
                .expect("scheduler state should lock");
            state.lifecycle = DelayedTaskSchedulerLifecycle::Shutdown;
        }

        assert!(matches!(
            scheduler.schedule(Duration::ZERO, test_noop_task),
            Err(RejectedExecution::Shutdown),
        ));
    }

    #[test]
    fn test_delayed_scheduler_loop_handles_empty_action_and_stopping() {
        let inner = Arc::new(DelayedTaskSchedulerInner::new());
        {
            let mut state = inner.state.lock().expect("scheduler state should lock");
            state.tasks.push(ScheduledTask {
                deadline: Instant::now(),
                sequence: 0,
                state: Arc::new(AtomicU8::new(TASK_STARTED)),
                task: Some(Box::new(test_noop_task)),
            });
            state.tasks.push(ScheduledTask {
                deadline: Instant::now(),
                sequence: 1,
                state: Arc::new(AtomicU8::new(TASK_PENDING)),
                task: None,
            });
            inner.queued_task_count.store(1, Ordering::Release);
            inner.condition.notify_all();
        }
        let worker_inner = Arc::clone(&inner);
        let worker = thread::spawn(move || run_delayed_scheduler(worker_inner));

        wait_until(|| inner.queued_count() == 0);
        inner.shutdown_now();
        worker
            .join()
            .expect("scheduler worker should exit after shutdown_now");
        assert!(inner.is_terminated());
    }

    #[test]
    fn test_delayed_task_scheduler_public_shutdown_now_and_rejection_paths() {
        let scheduler =
            DelayedTaskScheduler::new("test-delayed-scheduler-shutdown-now").expect("scheduler");
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new(AtomicBool::new(false));
        let release_for_task = Arc::clone(&release);

        scheduler
            .schedule(Duration::ZERO, move || {
                started_tx.send(()).expect("task should report start");
                while !release_for_task.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(1));
                }
            })
            .expect("running task should schedule");
        let pending = scheduler
            .schedule(Duration::from_secs(60), test_noop_task)
            .expect("pending task should schedule");
        assert!(scheduler.queued_count() >= 1);
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("running task should start");
        assert!(!scheduler.is_shutdown());

        let report = scheduler.shutdown_now();
        release.store(true, Ordering::Release);
        create_runtime().block_on(scheduler.await_termination());

        assert!(scheduler.is_shutdown());
        assert!(scheduler.is_terminated());
        assert_eq!(report.running, 1);
        assert_eq!(report.cancelled, 1);
        assert!(pending.is_cancelled());
        assert!(matches!(
            scheduler.schedule(Duration::ZERO, test_noop_task),
            Err(RejectedExecution::Shutdown),
        ));
    }

    #[test]
    fn test_delayed_scheduler_catches_panics_and_continues() {
        let scheduler =
            DelayedTaskScheduler::new("test-delayed-scheduler-panic").expect("scheduler");
        let (sent_tx, sent_rx) = mpsc::channel();
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        scheduler
            .schedule(Duration::ZERO, || {
                panic!("scheduled task panic");
            })
            .expect("panicking task should schedule");
        scheduler
            .schedule(Duration::from_millis(10), move || {
                sent_tx.send(()).expect("second task should send");
            })
            .expect("second task should schedule");

        sent_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("scheduler should continue after panic");
        std::panic::set_hook(previous_hook);
        scheduler.shutdown();
        create_runtime().block_on(scheduler.await_termination());
    }
}
