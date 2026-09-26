// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;

mod internal;

use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_executor::service::StopReport;
use qubit_executor::service::SubmissionError;
use qubit_lock::ParkingLotMonitor;
use qubit_lock::ParkingLotMonitorGuard;
#[cfg(feature = "async-wait")]
use tokio::sync::watch;

pub(super) use self::internal::ReservedWorker;
pub(super) use self::internal::ThreadPoolSubmitGuard;
use super::thread_pool_config::ThreadPoolConfig;
use super::thread_pool_state::ThreadPoolState;
use super::thread_pool_worker::ThreadPoolWorker;
use crate::ExecutorServiceBuilderError;
use crate::PoolJob;
use crate::PoolJobSubmissionError;
use crate::ThreadPoolHooks;
use crate::ThreadPoolStats;
use crate::internal::PoolAccounting;
use crate::pool_job::QueuedJob;
use crate::pool_job::QueuedJobState;

/// Shared state for a thread pool.
pub(crate) struct ThreadPoolInner {
    /// Lifecycle and worker state protected by a monitor.
    state_monitor: ParkingLotMonitor<ThreadPoolState>,
    /// Shared admission and task counters.
    accounting: PoolAccounting,
    /// Whether immediate shutdown has requested workers to stop taking jobs.
    stop_now: AtomicBool,
    /// Successfully spawned workers that have not exited yet.
    live_worker_count: AtomicUsize,
    /// Number of workers currently blocked or about to block waiting for work.
    idle_worker_count: AtomicUsize,
    /// Number of idle-worker wakeups already requested but not yet consumed.
    pending_worker_wakes: AtomicUsize,
    /// Number of callers waiting for in-flight submitters to leave admission.
    submit_waiter_count: AtomicUsize,
    /// Number of callers waiting for accepted work to become idle.
    idle_waiter_count: AtomicUsize,
    /// Removable FIFO; always lock this before an entry ownership mutex.
    global_queue: Mutex<VecDeque<Arc<QueuedJob>>>,
    /// Prefix used for naming newly spawned workers.
    thread_name_prefix: String,
    /// Optional stack size in bytes for newly spawned workers.
    stack_size: Option<usize>,
    /// Worker and task lifecycle hooks.
    hooks: ThreadPoolHooks,
    /// Monotonic termination notification for asynchronous waiters.
    #[cfg(feature = "async-wait")]
    termination_tx: watch::Sender<bool>,
    /// Signals changes that may allow a bounded submission to be retried.
    #[cfg(feature = "async-wait")]
    capacity_tx: watch::Sender<u64>,
}

impl ThreadPoolInner {
    /// Creates shared state for a thread pool.
    ///
    /// # Parameters
    ///
    /// * `config` - Initial immutable and mutable pool configuration.
    ///
    /// # Returns
    ///
    /// A shared-state object ready to accept worker and queue operations.
    pub(super) fn new(config: ThreadPoolConfig, hooks: ThreadPoolHooks) -> Self {
        let mut config = config;
        let thread_name_prefix = std::mem::take(&mut config.thread_name_prefix);
        let stack_size = config.stack_size;
        let queue_capacity = config.queue_capacity;
        Self {
            state_monitor: ParkingLotMonitor::new(ThreadPoolState::new(config)),
            accounting: PoolAccounting::new(queue_capacity),
            stop_now: AtomicBool::new(false),
            live_worker_count: AtomicUsize::new(0),
            idle_worker_count: AtomicUsize::new(0),
            pending_worker_wakes: AtomicUsize::new(0),
            submit_waiter_count: AtomicUsize::new(0),
            idle_waiter_count: AtomicUsize::new(0),
            global_queue: Mutex::new(VecDeque::new()),
            thread_name_prefix,
            stack_size,
            hooks,
            #[cfg(feature = "async-wait")]
            termination_tx: watch::channel(false).0,
            #[cfg(feature = "async-wait")]
            capacity_tx: watch::channel(0).0,
        }
    }

    /// Subscribes to the terminal state without losing a concurrent transition.
    #[cfg(feature = "async-wait")]
    pub(crate) fn subscribe_termination(&self) -> watch::Receiver<bool> {
        let state = self.lock_state();
        self.publish_termination_locked(&state);
        self.termination_tx.subscribe()
    }

    /// Subscribes to events that may change queue admission.
    #[cfg(feature = "async-wait")]
    pub(crate) fn capacity_changes(&self) -> watch::Receiver<u64> {
        self.capacity_tx.subscribe()
    }

    /// Publishes a possibly available queue slot or a lifecycle change.
    #[cfg(feature = "async-wait")]
    fn notify_capacity_changed(&self) {
        self.capacity_tx
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// Publishes terminal state while the caller holds the pool state lock.
    #[cfg(feature = "async-wait")]
    fn publish_termination_locked(&self, state: &ThreadPoolState) {
        if self.is_terminated_locked(state) {
            self.termination_tx.send_replace(true);
        }
    }

    /// Returns the hook set used by worker threads.
    ///
    /// # Returns
    ///
    /// Worker and task lifecycle hooks.
    #[inline]
    pub(crate) fn hooks(&self) -> &ThreadPoolHooks {
        &self.hooks
    }

    /// Returns the accepted queued work count.
    ///
    /// # Returns
    ///
    /// Number of accepted jobs waiting to start.
    #[inline]
    pub(crate) fn queued_count(&self) -> usize {
        self.accounting.queued_count()
    }

    /// Returns the running work count.
    ///
    /// # Returns
    ///
    /// Number of jobs currently held by workers.
    #[inline]
    pub(crate) fn running_count(&self) -> usize {
        self.accounting.running_count()
    }

    /// Returns the number of submit calls currently inside admission.
    ///
    /// # Returns
    ///
    /// Number of submitters that may still publish, spawn, or roll back work.
    #[inline]
    pub(crate) fn inflight_count(&self) -> usize {
        self.accounting.inflight_count()
    }

    /// Acquires the pool state monitor while tolerating poisoned locks.
    ///
    /// # Returns
    ///
    /// A monitor guard for the mutable pool state.
    #[inline]
    pub(crate) fn lock_state(&self) -> ParkingLotMonitorGuard<'_, ThreadPoolState> {
        self.state_monitor.lock()
    }

    /// Acquires the pool state and reads it while holding the monitor lock.
    ///
    /// # Arguments
    ///
    /// * `f` - Closure that reads the state.
    ///
    /// # Returns
    ///
    /// The value returned by the closure.
    #[inline]
    pub(crate) fn read_state<R, F>(&self, f: F) -> R
    where
        F: FnOnce(&ThreadPoolState) -> R,
    {
        self.state_monitor.with_read(f)
    }

    /// Acquires the pool state and mutates it while holding the monitor lock.
    ///
    /// # Arguments
    ///
    /// * `f` - Closure that mutates the state.
    ///
    /// # Returns
    ///
    /// The value returned by the closure.
    #[inline]
    pub(crate) fn write_state<R, F>(&self, f: F) -> R
    where
        F: FnOnce(&mut ThreadPoolState) -> R,
    {
        self.state_monitor.with_write(f)
    }

    /// Attempts to enter submit admission.
    ///
    /// # Returns
    ///
    /// A guard that leaves admission when dropped.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] when admission is already closed.
    fn begin_submit(&self) -> Result<ThreadPoolSubmitGuard<'_>, SubmissionError> {
        if self.accounting.try_enter() {
            Ok(ThreadPoolSubmitGuard { inner: self })
        } else {
            Err(SubmissionError::Shutdown)
        }
    }

    /// Returns whether any caller is waiting for accepted work to drain.
    ///
    /// # Returns
    ///
    /// `true` when the last in-flight submitter should wake idle waiters.
    fn has_idle_waiters(&self) -> bool {
        self.idle_waiter_count.load(Ordering::Acquire) > 0
    }

    /// Wakes monitor waiters after the last submitter leaves admission.
    fn notify_submitter_departure(&self) {
        if !self.accounting.is_admission_open() || self.has_idle_waiters() {
            self.notify_waiters_after_atomic_change();
        }
    }

    /// Attempts to reserve one queued-work slot.
    ///
    /// # Returns
    ///
    /// `true` when a slot was reserved.
    fn reserve_queue_slot(&self) -> bool {
        self.accounting.try_reserve_bounded_slot()
    }

    /// Reserves a queued-work slot for a successfully spawned worker's job.
    ///
    /// Worker growth admits this job even when the ordinary queue is full.
    /// The slot is released when acceptance fails, execution starts, or
    /// cancellation completes.
    fn reserve_worker_handoff_slot(&self) {
        self.accounting.reserve_worker_handoff_slot();
    }

    /// Releases one reserved queued-work slot that never became runnable.
    fn release_queue_slot(&self) {
        self.accounting.rollback_reserved_slot();
        #[cfg(feature = "async-wait")]
        self.notify_capacity_changed();
    }

    /// Accepts a job whose queue slot has already been reserved and publishes
    /// it.
    ///
    /// This method runs custom acceptance callbacks on the submitting thread
    /// and therefore must not be called while holding the pool-state monitor.
    ///
    /// # Parameters
    ///
    /// * `job` - Job with one reserved queue slot.
    ///
    /// # Returns
    ///
    /// `Ok(())` after acceptance succeeds and the job is published.
    ///
    /// # Errors
    ///
    /// Returns [`PoolJobSubmissionError::AcceptancePanicked`] if the acceptance
    /// callback panics, releasing the reserved slot without publishing the job.
    fn accept_and_enqueue_reserved_job(&self, mut job: PoolJob) -> Result<(), PoolJobSubmissionError> {
        let entry = job.take_queue_entry();
        if let Some(entry) = &entry {
            *entry.lock() = QueuedJobState::Accepting {
                owner: thread::current().id(),
                cancel_requested: false,
            };
        }
        if job.accept().is_err() {
            self.release_queue_slot();
            if let Some(entry) = &entry {
                *entry.lock() = QueuedJobState::Rejected;
                entry.changed.notify_all();
            }
            self.notify_waiters_after_atomic_change();
            job.discard();
            return Err(PoolJobSubmissionError::AcceptancePanicked);
        }
        let mut queue = self
            .global_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.accounting.publish_accepted_job();
        if let Some(entry) = entry {
            let mut state = entry.lock();
            if matches!(
                *state,
                QueuedJobState::Accepting {
                    cancel_requested: true,
                    ..
                }
            ) {
                *state = QueuedJobState::Cancelling;
                self.begin_cancel_queued_job();
                drop(state);
                drop(queue);
                job.cancel();
                self.finish_cancelled_job();
                *entry.lock() = QueuedJobState::Cancelled;
                entry.changed.notify_all();
                return Ok(());
            }
            *state = QueuedJobState::Queued(job);
            drop(state);
            queue.push_back(entry);
        } else {
            queue.push_back(Arc::new(QueuedJob::new(job)));
        }
        drop(queue);
        self.wake_one_idle_worker();
        Ok(())
    }

    /// Removes `entry` and cancels its callbacks once, returning whether it
    /// won. Queue removal and accounting occur before invoking user code
    /// outside locks.
    pub(crate) fn cancel_queued_entry(&self, entry: &Arc<QueuedJob>) -> bool {
        let job = {
            let mut queue = self
                .global_queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(index) = queue.iter().position(|queued| Arc::ptr_eq(queued, entry)) else {
                return false;
            };
            let Some(job) = entry.take_queued() else {
                return false;
            };
            queue.remove(index);
            self.begin_cancel_queued_job();
            job
        };
        job.cancel();
        self.finish_cancelled_job();
        true
    }

    /// Submits a job into the queue.
    ///
    /// # Overall logic
    ///
    /// This method performs dynamic admission while holding the pool-state
    /// monitor when it must inspect worker capacity or lifecycle state:
    ///
    /// 1. Reject immediately if the lifecycle is not running.
    /// 2. If live workers are below the core size or no workers remain, spawn a
    ///    worker and reserve a queue slot independently of queue capacity.
    /// 3. Otherwise, try enqueuing the job if the queue is not saturated.
    /// 4. If the queue is saturated but live workers are still below maximum,
    ///    spawn a non-core worker and reserve a slot independently of capacity.
    /// 5. Otherwise reject as saturated.
    ///
    /// After admission, acceptance runs on the submitting thread outside the
    /// monitor, then the job is published to the global queue. Worker-spawn
    /// failure rejects the job before its acceptance callback runs.
    ///
    /// Queued submissions use a targeted wake-up strategy with pending wake
    /// tokens, so submitters wake at most one idle worker while avoiding the
    /// lost-notification window around worker parking.
    ///
    /// # Parameters
    ///
    /// * `job` - Type-erased job to execute or cancel later.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the job is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] after shutdown, returns
    /// [`SubmissionError::Saturated`] when the queue and worker capacity are
    /// full, or returns [`SubmissionError::WorkerSpawnFailed`] if a required
    /// worker cannot be created. These errors are wrapped in
    /// [`PoolJobSubmissionError::Rejected`]. Returns
    /// [`PoolJobSubmissionError::AcceptancePanicked`] if acceptance panics.
    pub(crate) fn submit(self: &Arc<Self>, job: PoolJob) -> Result<(), PoolJobSubmissionError> {
        job.assert_pool(self);
        let _guard = match self.begin_submit() {
            Ok(guard) => guard,
            Err(error) => {
                job.discard();
                return Err(error.into());
            }
        };
        self.submit_with_state_lock(job)
    }

    /// Submits a job through the locked dynamic admission path.
    ///
    /// # Parameters
    ///
    /// * `job` - Job to admit and publish after releasing the monitor.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the job is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`], [`SubmissionError::Saturated`],
    /// or [`SubmissionError::WorkerSpawnFailed`] according to the dynamic
    /// admission state observed under the monitor.
    fn submit_with_state_lock(self: &Arc<Self>, job: PoolJob) -> Result<(), PoolJobSubmissionError> {
        if let Err(error) = self.reserve_submission_with_state_lock() {
            // Destructors are user code too: release the state monitor first.
            job.discard();
            return Err(error);
        }
        self.accept_and_enqueue_reserved_job(job)
    }

    /// Reserves admission capacity under the state monitor without taking job
    /// ownership, so rejected captures are always destroyed outside the lock.
    /// Returns Shutdown, Saturated, or WorkerSpawnFailed if reservation fails.
    fn reserve_submission_with_state_lock(self: &Arc<Self>) -> Result<(), PoolJobSubmissionError> {
        let mut state = self.lock_state();
        if state.lifecycle != ExecutorServiceLifecycle::Running {
            return Err(PoolJobSubmissionError::Rejected(SubmissionError::Shutdown));
        }
        if state.live_workers < state.core_pool_size {
            let worker = self.reserve_worker_locked(&mut state);
            self.spawn_reserved_worker_locked(&mut state, worker)?;
            self.reserve_worker_handoff_slot();
            return Ok(());
        }
        if state.live_workers == 0 {
            let worker = self.reserve_worker_locked(&mut state);
            self.spawn_reserved_worker_locked(&mut state, worker)?;
            self.reserve_worker_handoff_slot();
            return Ok(());
        }
        if self.reserve_queue_slot() {
            return Ok(());
        }
        if state.live_workers < state.maximum_pool_size {
            let worker = self.reserve_worker_locked(&mut state);
            self.spawn_reserved_worker_locked(&mut state, worker)?;
            self.reserve_worker_handoff_slot();
            Ok(())
        } else {
            Err(PoolJobSubmissionError::Rejected(SubmissionError::Saturated))
        }
    }

    /// Starts one missing core worker.
    ///
    /// # Returns
    ///
    /// `Ok(true)` when a worker was spawned, or `Ok(false)` when the core
    /// pool size is already satisfied.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] after shutdown or
    /// [`SubmissionError::WorkerSpawnFailed`] if the worker cannot be
    /// created.
    pub(crate) fn prestart_core_thread(self: &Arc<Self>) -> Result<bool, SubmissionError> {
        let mut state = self.lock_state();
        if state.lifecycle != ExecutorServiceLifecycle::Running {
            return Err(SubmissionError::Shutdown);
        }
        if state.live_workers >= state.core_pool_size {
            return Ok(false);
        }
        let worker = self.reserve_worker_locked(&mut state);
        self.spawn_reserved_worker_locked(&mut state, worker)?;
        Ok(true)
    }

    /// Starts all missing core workers.
    ///
    /// # Returns
    ///
    /// The number of workers started.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError`] if shutdown is observed or a worker cannot
    /// be created.
    pub(crate) fn prestart_all_core_threads(self: &Arc<Self>) -> Result<usize, SubmissionError> {
        let mut started = 0;
        while self.prestart_core_thread()? {
            started += 1;
        }
        Ok(started)
    }

    /// Reserves a worker while the caller holds the pool state lock.
    ///
    /// # Parameters
    ///
    /// * `state` - Locked mutable pool state to update while spawning.
    ///
    /// # Returns
    ///
    /// A worker reservation ready to spawn while the lock is still held.
    fn reserve_worker_locked(self: &Arc<Self>, state: &mut ThreadPoolState) -> ReservedWorker {
        let index = state.next_worker_index;
        state.next_worker_index += 1;
        state.live_workers += 1;
        ReservedWorker { index }
    }

    /// Spawns a previously reserved worker while holding the state lock.
    ///
    /// # Parameters
    ///
    /// * `state` - Locked pool state used for rollback on spawn failure.
    /// * `worker` - Worker reservation created while holding the state lock.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the worker thread is spawned.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::WorkerSpawnFailed`] if
    /// [`thread::Builder::spawn`] fails.
    fn spawn_reserved_worker_locked(
        self: &Arc<Self>,
        state: &mut ThreadPoolState,
        worker: ReservedWorker,
    ) -> Result<(), SubmissionError> {
        let ReservedWorker { index } = worker;
        let worker_inner = Arc::clone(self);
        let mut builder = thread::Builder::new().name(format!("{}-{index}", self.thread_name_prefix));
        if let Some(stack_size) = self.stack_size {
            builder = builder.stack_size(stack_size);
        }
        match builder.spawn(move || {
            ThreadPoolWorker::run(worker_inner, index);
        }) {
            Ok(_) => {
                self.live_worker_count.fetch_add(1, Ordering::Release);
                Ok(())
            }
            Err(source) => {
                state.live_workers = state
                    .live_workers
                    .checked_sub(1)
                    .expect("thread pool live worker counter underflow");
                self.notify_if_idle_or_terminated(state);
                Err(SubmissionError::WorkerSpawnFailed {
                    source: Arc::new(source),
                })
            }
        }
    }

    /// Attempts to take one queued job without acquiring the state monitor.
    ///
    /// # Returns
    ///
    /// `Some(job)` when the queue has work, otherwise `None`.
    pub(crate) fn try_take_queued_job(&self) -> Option<PoolJob> {
        let mut queue = self
            .global_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stop_now.load(Ordering::Acquire) {
            return None;
        }
        let entry = queue.pop_front()?;
        let job = entry.take_queued()?;
        self.accounting.claim_queued_job();
        #[cfg(feature = "async-wait")]
        self.notify_capacity_changed();
        Some(job)
    }

    /// Marks a worker as idle in lock-free wake-up state.
    pub(crate) fn mark_worker_idle(&self) {
        self.idle_worker_count.fetch_add(1, Ordering::AcqRel);
    }

    /// Marks a worker as no longer idle and consumes one pending wake token.
    pub(crate) fn unmark_worker_idle(&self) {
        let previous = self.idle_worker_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "thread pool idle worker counter underflow");
        self.consume_pending_worker_wake();
    }

    /// Wakes one idle worker if no already-requested wakeup covers it.
    ///
    /// Pending wake tokens close the lost-notification window between a worker
    /// marking itself idle and actually parking on the condition variable.
    fn wake_one_idle_worker(&self) {
        let idle_workers = self.idle_worker_count.load(Ordering::Acquire);
        if idle_workers == 0 {
            return;
        }
        let requested = self
            .pending_worker_wakes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending_wakes| {
                (pending_wakes < idle_workers).then_some(pending_wakes + 1)
            });
        if requested.is_ok() {
            self.lock_state().notify_one();
        }
    }

    /// Returns whether an idle-worker wakeup has been requested.
    ///
    /// # Returns
    ///
    /// `true` when a worker should retry taking work instead of parking.
    pub(crate) fn has_pending_worker_wake(&self) -> bool {
        self.pending_worker_wakes.load(Ordering::Acquire) > 0
    }

    /// Consumes one pending idle-worker wakeup if one exists.
    fn consume_pending_worker_wake(&self) {
        let _ = self
            .pending_worker_wakes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| current.checked_sub(1));
    }

    /// Opens cancellation accounting for one queued job.
    fn begin_cancel_queued_job(&self) {
        self.accounting.begin_cancel_queued_job();
    }

    /// Drains all jobs currently visible in the global queue.
    ///
    /// # Returns
    ///
    /// Drained queued jobs.
    fn drain_visible_queued_jobs(&self) -> Vec<PoolJob> {
        let mut queue = self
            .global_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut jobs = Vec::with_capacity(queue.len());
        while let Some(entry) = queue.pop_front() {
            if let Some(job) = entry.take_queued() {
                self.begin_cancel_queued_job();
                jobs.push(job);
            }
        }
        jobs
    }

    /// Marks one running job as finished.
    pub(crate) fn finish_running_job(&self) {
        self.accounting.finish_running_job();
        #[cfg(feature = "async-wait")]
        self.notify_capacity_changed();
        if self.accounting.is_idle() {
            self.notify_waiters_after_atomic_change();
        }
    }

    /// Requests graceful shutdown.
    ///
    /// The pool rejects later submissions but lets queued work drain.
    pub(crate) fn shutdown(&self) {
        self.accounting.close_admission();
        #[cfg(feature = "async-wait")]
        self.notify_capacity_changed();
        let mut state = self.lock_state();
        if state.lifecycle == ExecutorServiceLifecycle::Running {
            state.lifecycle = ExecutorServiceLifecycle::ShuttingDown;
        }
        #[cfg(feature = "async-wait")]
        self.publish_termination_locked(&state);
        state.notify_all();
    }

    /// Requests abrupt shutdown and cancels queued jobs.
    ///
    /// The report's `queued` and `cancelled` counts include only jobs that
    /// this call removes from the queue and cancels. A concurrent ticket
    /// cancellation that removes a job first is not included. The `running`
    /// count is a snapshot taken after in-flight submissions have finished.
    ///
    /// # Returns
    ///
    /// A report containing jobs cancelled by this stop call and a snapshot of
    /// jobs running after in-flight submissions have finished.
    pub(crate) fn stop(&self) -> StopReport {
        self.accounting.close_admission();
        #[cfg(feature = "async-wait")]
        self.notify_capacity_changed();
        self.stop_now.store(true, Ordering::Release);
        let (jobs, queued, running) = {
            let mut state = self.lock_state();
            self.stop_now.store(true, Ordering::Release);
            if matches!(
                state.lifecycle,
                ExecutorServiceLifecycle::Running | ExecutorServiceLifecycle::ShuttingDown
            ) {
                state.lifecycle = ExecutorServiceLifecycle::Stopping;
            }
            if self.inflight_count() > 0 {
                self.submit_waiter_count.fetch_add(1, Ordering::AcqRel);
                while self.inflight_count() > 0 {
                    state.wait();
                }
                let previous = self.submit_waiter_count.fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "thread pool submit waiter counter underflow");
            }
            let running = self.running_count();
            let jobs = self.drain_visible_queued_jobs();
            let drained = jobs.len();
            #[cfg(feature = "async-wait")]
            self.publish_termination_locked(&state);
            state.notify_all();
            (jobs, drained, running)
        };
        for job in jobs {
            job.cancel();
            self.finish_cancelled_job();
        }
        self.lock_state().notify_all();
        StopReport::new(queued, running, queued)
    }

    /// Marks one queued-job cancellation callback as completed.
    ///
    /// This method closes the queue slot only after the callback returns, so
    /// join and termination waiters cannot observe a cancelled job as fully
    /// inactive while user cancellation code is still running.
    fn finish_cancelled_job(&self) {
        self.accounting.finish_cancelled_job();
        #[cfg(feature = "async-wait")]
        self.notify_capacity_changed();
        if self.is_idle_snapshot() {
            self.notify_waiters_after_atomic_change();
        }
    }

    /// Returns whether shutdown has been requested.
    ///
    /// # Returns
    ///
    /// `true` if the pool is no longer in the running lifecycle state.
    pub(crate) fn is_not_running(&self) -> bool {
        self.read_state(|state| state.lifecycle != ExecutorServiceLifecycle::Running)
    }

    /// Returns the current lifecycle state.
    ///
    /// # Returns
    ///
    /// [`ExecutorServiceLifecycle::Terminated`] after all accepted work and
    /// workers are gone, otherwise the stored lifecycle state.
    pub(crate) fn lifecycle(&self) -> ExecutorServiceLifecycle {
        self.read_state(|state| {
            if self.is_terminated_locked(state) {
                ExecutorServiceLifecycle::Terminated
            } else {
                state.lifecycle
            }
        })
    }

    /// Returns whether the pool is fully terminated.
    ///
    /// # Returns
    ///
    /// `true` if shutdown has started and no queued, running, or live worker
    /// state remains.
    pub(crate) fn is_terminated(&self) -> bool {
        self.read_state(|state| self.is_terminated_locked(state))
    }

    /// Blocks the current thread until this pool is terminated.
    ///
    /// This method waits on a condition variable and therefore blocks the
    /// calling thread.
    pub(crate) fn wait_for_termination(&self) {
        self.state_monitor
            .wait_until_ready(|state| self.is_terminated_locked(state));
    }

    /// Waits for termination for at most `timeout`.
    pub(crate) fn wait_for_termination_timeout(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return self.is_terminated();
            }
            match self
                .state_monitor
                .wait_until_ready_with_total_timeout(remaining.min(Duration::from_secs(3600)), |state| {
                    self.is_terminated_locked(state)
                }) {
                Ok(result) if result.is_ready() => return true,
                Ok(_) => {}
                Err(_) => return self.is_terminated(),
            }
        }
    }

    /// Blocks until all currently accepted work has completed.
    ///
    /// This method waits for queued and running tasks to drain, but it does not
    /// request shutdown and does not wait for worker threads to exit.
    pub(crate) fn wait_until_idle(&self) {
        self.idle_waiter_count.fetch_add(1, Ordering::AcqRel);
        let mut state = self.lock_state();
        while !self.is_idle_snapshot() {
            state.wait();
        }
        let previous = self.idle_waiter_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "thread pool idle waiter counter underflow");
    }

    /// Returns a best-effort pool snapshot.
    ///
    /// # Returns
    ///
    /// Independently loaded task counters with monitor-protected worker state.
    pub(crate) fn stats(&self) -> ThreadPoolStats {
        let counters = self.accounting.snapshot();
        self.read_state(|state| {
            let terminated = self.is_terminated_locked(state);
            ThreadPoolStats {
                lifecycle: if terminated {
                    ExecutorServiceLifecycle::Terminated
                } else {
                    state.lifecycle
                },
                core_pool_size: state.core_pool_size,
                maximum_pool_size: state.maximum_pool_size,
                live_workers: state.live_workers,
                idle_workers: state.idle_workers,
                queued_tasks: counters.queued_tasks,
                queue_capacity: self.accounting.queue_capacity(),
                running_tasks: counters.running_tasks,
                submitted_tasks: counters.submitted_tasks,
                completed_tasks: counters.completed_tasks,
                cancelled_tasks: counters.cancelled_tasks,
                terminated,
            }
        })
    }

    /// Updates the core pool size.
    ///
    /// # Parameters
    ///
    /// * `core_pool_size` - New core pool size.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the value is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorServiceBuilderError::CorePoolSizeExceedsMaximum`] when
    /// the new core size is greater than the current maximum size.
    pub(crate) fn set_core_pool_size(
        self: &Arc<Self>,
        core_pool_size: usize,
    ) -> Result<(), ExecutorServiceBuilderError> {
        let err = self.write_state(|state| {
            if core_pool_size > state.maximum_pool_size {
                Some(state.maximum_pool_size)
            } else {
                state.core_pool_size = core_pool_size;
                None
            }
        });
        if let Some(maximum_pool_size) = err {
            return Err(ExecutorServiceBuilderError::CorePoolSizeExceedsMaximum {
                core_pool_size,
                maximum_pool_size,
            });
        }
        self.state_monitor.notify_all();
        Ok(())
    }

    /// Updates the maximum pool size.
    ///
    /// # Parameters
    ///
    /// * `maximum_pool_size` - New maximum pool size.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the value is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorServiceBuilderError::ZeroMaximumPoolSize`] for zero,
    /// or [`ExecutorServiceBuilderError::CorePoolSizeExceedsMaximum`] when
    /// the current core size is greater than the new maximum size.
    pub(crate) fn set_maximum_pool_size(
        self: &Arc<Self>,
        maximum_pool_size: usize,
    ) -> Result<(), ExecutorServiceBuilderError> {
        if maximum_pool_size == 0 {
            return Err(ExecutorServiceBuilderError::ZeroMaximumPoolSize);
        }
        let exceeds = self.write_state(|state| {
            if state.core_pool_size > maximum_pool_size {
                Some(state.core_pool_size)
            } else {
                state.maximum_pool_size = maximum_pool_size;
                None
            }
        });
        if let Some(core_pool_size) = exceeds {
            return Err(ExecutorServiceBuilderError::CorePoolSizeExceedsMaximum {
                core_pool_size,
                maximum_pool_size,
            });
        }
        self.state_monitor.notify_all();
        Ok(())
    }

    /// Updates the worker keep-alive timeout.
    ///
    /// # Parameters
    ///
    /// * `keep_alive` - New idle timeout.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the timeout is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorServiceBuilderError::ZeroKeepAlive`] when the duration
    /// is zero.
    pub(crate) fn set_keep_alive(&self, keep_alive: Duration) -> Result<(), ExecutorServiceBuilderError> {
        if keep_alive.is_zero() {
            return Err(ExecutorServiceBuilderError::ZeroKeepAlive);
        }
        self.state_monitor
            .with_write_notify_all(|state| state.keep_alive = keep_alive);
        Ok(())
    }

    /// Updates whether idle core workers may time out.
    ///
    /// # Parameters
    ///
    /// * `allow` - Whether idle core workers may retire after keep-alive.
    pub(crate) fn allow_core_thread_timeout(&self, allow: bool) {
        self.state_monitor.with_write_notify_all(|state| {
            state.allow_core_thread_timeout = allow;
        });
    }

    /// Checks whether all accepted work has drained.
    ///
    /// # Returns
    ///
    /// `true` when no queued slot, running job, or cancellation callback
    /// remains.
    fn is_idle_snapshot(&self) -> bool {
        self.accounting.is_idle()
    }

    /// Checks termination against one locked lifecycle snapshot.
    ///
    /// # Parameters
    ///
    /// * `state` - Locked lifecycle and worker state.
    ///
    /// # Returns
    ///
    /// `true` when shutdown has started and no workers or jobs remain active.
    fn is_terminated_locked(&self, state: &ThreadPoolState) -> bool {
        state.lifecycle != ExecutorServiceLifecycle::Running && state.live_workers == 0 && self.is_idle_snapshot()
    }

    /// Notifies waiters after an atomic-only condition change.
    fn notify_waiters_after_atomic_change(&self) {
        let state = self.lock_state();
        #[cfg(feature = "async-wait")]
        self.publish_termination_locked(&state);
        state.notify_all();
    }

    /// Notifies termination waiters when the state is terminal.
    ///
    /// # Parameters
    ///
    /// * `state` - Current pool state observed while holding the state lock.
    pub(crate) fn notify_if_terminated(&self, state: &ThreadPoolState) {
        if self.is_terminated_locked(state) {
            #[cfg(feature = "async-wait")]
            self.publish_termination_locked(state);
            self.state_monitor.notify_all();
        }
    }

    /// Notifies waiters when the pool is idle or fully terminated.
    ///
    /// # Parameters
    ///
    /// * `state` - Current pool state observed while holding the state lock.
    pub(crate) fn notify_if_idle_or_terminated(&self, state: &ThreadPoolState) {
        if self.is_idle_snapshot() || self.is_terminated_locked(state) {
            #[cfg(feature = "async-wait")]
            self.publish_termination_locked(state);
            self.state_monitor.notify_all();
        }
    }

    /// Marks one worker as exited while the caller holds the state lock.
    ///
    /// # Parameters
    ///
    /// * `state` - Locked mutable pool state whose live count is decremented.
    pub(crate) fn unregister_worker_locked(&self, state: &mut ThreadPoolState) {
        state.live_workers = state
            .live_workers
            .checked_sub(1)
            .expect("thread pool live worker counter underflow");
        let previous = self.live_worker_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "thread pool live worker counter underflow");
        self.notify_if_terminated(state);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;

    use qubit_executor::service::ExecutorServiceLifecycle;
    use qubit_executor::service::SubmissionError;

    use super::ThreadPoolConfig;
    use super::ThreadPoolHooks;
    use super::ThreadPoolInner;
    use crate::PoolJob;
    use crate::PoolJobSubmissionError;

    #[test]
    fn test_stop_report_excludes_ticket_cancellation_after_stop_snapshot() {
        let inner = Arc::new(ThreadPoolInner::new(
            ThreadPoolConfig {
                core_pool_size: 1,
                maximum_pool_size: 1,
                queue_capacity: Some(1),
                thread_name_prefix: String::from("stop-report-test"),
                stack_size: None,
                keep_alive: Duration::from_secs(1),
                allow_core_thread_timeout: false,
            },
            ThreadPoolHooks::default(),
        ));
        let (worker_started_tx, worker_started_rx) = mpsc::channel();
        let (release_worker_tx, release_worker_rx) = mpsc::channel();
        inner
            .submit(PoolJob::new(
                Box::new(move || {
                    worker_started_tx.send(()).expect("worker should start");
                    release_worker_rx.recv().expect("worker should be released");
                }),
                Box::new(|| {}),
            ))
            .expect("blocking job should be accepted");
        worker_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should start");

        let (cancel_started_tx, cancel_started_rx) = mpsc::channel();
        let (release_cancel_tx, release_cancel_rx) = mpsc::channel();
        let (job, ticket) = PoolJob::prepare_cancellable(
            &inner,
            Box::new(|| {}),
            Box::new(|| panic!("cancelled job must not run")),
            Box::new(move || {
                cancel_started_tx.send(()).expect("cancellation should start");
                release_cancel_rx.recv().expect("cancellation should be released");
            }),
        );
        inner.submit(job).expect("ticketed job should be accepted");

        // Hold stop after its report baseline is taken, then race an independent
        // ticket cancellation into the old implementation's counter delta.
        assert!(inner.accounting.try_enter(), "in-flight gate should open");
        let stop_inner = Arc::clone(&inner);
        let stop_thread = thread::spawn(move || stop_inner.stop());
        let deadline = Instant::now() + Duration::from_secs(2);
        while (!inner.stop_now.load(Ordering::Acquire) || inner.submit_waiter_count.load(Ordering::Acquire) == 0)
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        assert!(
            inner.stop_now.load(Ordering::Acquire) && inner.submit_waiter_count.load(Ordering::Acquire) > 0,
            "stop should wait after publishing its stop request",
        );

        let cancel_thread = thread::spawn(move || ticket.cancel_queued());
        cancel_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ticket cancellation should start after stop's snapshot");

        let _was_last_submitter = inner.accounting.leave();
        inner.notify_waiters_after_atomic_change();
        let report = stop_thread
            .join()
            .expect("stop should return a report without the ticket cancellation");
        assert_eq!(report.queued, 0);
        assert_eq!(report.cancelled, 0);
        assert_eq!(report.running, 1);

        release_cancel_tx.send(()).expect("cancellation should be released");
        assert!(cancel_thread.join().expect("ticket cancellation should finish"));
        release_worker_tx.send(()).expect("worker should be released");
        assert!(inner.wait_for_termination_timeout(Duration::from_secs(2)));
    }

    #[test]
    fn test_submit_with_state_lock_rejects_stopping_state() {
        let inner = std::sync::Arc::new(ThreadPoolInner::new(
            ThreadPoolConfig {
                core_pool_size: 1,
                maximum_pool_size: 1,
                queue_capacity: Some(1),
                thread_name_prefix: String::from("test"),
                stack_size: None,
                keep_alive: Duration::from_secs(1),
                allow_core_thread_timeout: false,
            },
            ThreadPoolHooks::default(),
        ));
        inner.lock_state().lifecycle = ExecutorServiceLifecycle::Stopping;
        let result = inner.submit_with_state_lock(PoolJob::new(
            Box::new(|| panic!("rejected job must not run")),
            Box::new(|| panic!("rejected job must not be cancelled")),
        ));
        assert!(matches!(
            result,
            Err(PoolJobSubmissionError::Rejected(SubmissionError::Shutdown))
        ));
    }

    #[test]
    fn test_concurrent_core_size_updates_keep_atomic_mirror_in_sync() {
        let inner = Arc::new(ThreadPoolInner::new(
            ThreadPoolConfig {
                core_pool_size: 1,
                maximum_pool_size: 2,
                queue_capacity: Some(1),
                thread_name_prefix: String::from("core-size-test"),
                stack_size: None,
                keep_alive: Duration::from_secs(1),
                allow_core_thread_timeout: false,
            },
            ThreadPoolHooks::default(),
        ));
        let start = Barrier::new(3);
        let done = Barrier::new(3);
        thread::scope(|scope| {
            for value in [1, 2] {
                let inner = Arc::clone(&inner);
                let start = &start;
                let done = &done;
                scope.spawn(move || {
                    for _ in 0..5_000 {
                        start.wait();
                        inner.set_core_pool_size(value).expect("valid core size");
                        done.wait();
                    }
                });
            }
            for _ in 0..5_000 {
                start.wait();
                done.wait();
                inner.read_state(|state| {
                    assert!(matches!(state.core_pool_size, 1 | 2));
                    assert!(state.core_pool_size <= state.maximum_pool_size);
                });
            }
        });
    }
}
