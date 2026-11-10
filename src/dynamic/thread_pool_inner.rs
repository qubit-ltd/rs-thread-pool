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
use qubit_executor::service::RejectedTask;
use qubit_executor::service::StopReport;
use qubit_executor::service::SubmissionError;
use qubit_executor::task::spi::TaskEndpointPair;
use qubit_function::Callable;
use qubit_lock::ParkingLotMonitor;
use qubit_lock::ParkingLotMonitorGuard;
#[cfg(feature = "async-wait")]
use tokio::sync::watch;

pub(super) use self::internal::AcceptedJobGuard;
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
    /// Test-only switch used to exercise reservation unwind cleanup.
    #[cfg(test)]
    panic_after_worker_reservation: AtomicBool,
    /// Test-only switch for deterministic worker-spawn rejection coverage.
    #[cfg(test)]
    fail_next_worker_spawn: AtomicBool,
    /// One-pool deterministic notification scheduling control.
    #[cfg(test)]
    notification_control: Arc<crate::tests::notification_control::NotificationControl>,
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
            #[cfg(test)]
            panic_after_worker_reservation: AtomicBool::new(false),
            #[cfg(test)]
            fail_next_worker_spawn: AtomicBool::new(false),
            #[cfg(test)]
            notification_control: Arc::new(crate::tests::notification_control::NotificationControl::new()),
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
    ///
    /// # Returns
    ///
    /// A receiver that observes `true` after the pool reaches its terminal
    /// state.
    #[cfg(feature = "async-wait")]
    pub(crate) fn subscribe_termination(&self) -> watch::Receiver<bool> {
        let state = self.lock_state();
        self.publish_termination_locked(&state);
        self.termination_tx.subscribe()
    }

    /// Subscribes to events that may change queue admission.
    ///
    /// # Returns
    ///
    /// A receiver notified when queue capacity or lifecycle changes may allow
    /// a bounded submission to be retried.
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
    ///
    /// # Parameters
    ///
    /// * `state` - Locked pool lifecycle and worker state.
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
    #[must_use]
    #[inline]
    pub(crate) fn hooks(&self) -> &ThreadPoolHooks {
        &self.hooks
    }

    /// Returns the accepted queued work count.
    ///
    /// # Returns
    ///
    /// Number of accepted jobs waiting to start.
    #[must_use]
    #[inline]
    pub(crate) fn queued_count(&self) -> usize {
        self.accounting.queued_count()
    }

    /// Returns the running work count.
    ///
    /// # Returns
    ///
    /// Number of jobs currently held by workers.
    #[must_use]
    #[inline]
    pub(crate) fn running_count(&self) -> usize {
        self.accounting.running_count()
    }

    /// Returns the number of submit calls currently inside admission.
    ///
    /// # Returns
    ///
    /// Number of submitters that may still publish, spawn, or roll back work.
    #[must_use]
    #[inline]
    pub(crate) fn inflight_count(&self) -> usize {
        self.accounting.inflight_count()
    }

    #[cfg(test)]
    /// Returns this inner pool's deterministic notification controller.
    pub(crate) fn notification_control(&self) -> Arc<crate::tests::notification_control::NotificationControl> {
        Arc::clone(&self.notification_control)
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
    /// # Type Parameters
    ///
    /// * `R` - Value returned by the read closure.
    /// * `F` - Closure type.
    ///
    /// # Parameters
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
    /// # Type Parameters
    ///
    /// * `R` - Value returned by the write closure.
    /// * `F` - Closure type.
    ///
    /// # Parameters
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
        #[cfg(test)]
        self.notification_control
            .pause(crate::tests::notification_control::Phase::SubmitterDeparture);
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
    fn accept_and_enqueue_reserved_job(&self, job: PoolJob) -> Result<(), PoolJobSubmissionError> {
        self.accept_and_enqueue_reserved_job_with_guard(job, None)
    }

    /// Accepts and publishes a reserved job using its existing transaction.
    ///
    /// # Parameters
    ///
    /// * `job` - Job with one reserved queue slot.
    /// * `reserved_transaction` - Transaction guard created immediately after
    ///   reservation, or `None` when this method must create it.
    ///
    /// # Returns
    ///
    /// `Ok(())` after acceptance succeeds and the job is published.
    ///
    /// # Errors
    ///
    /// Returns [`PoolJobSubmissionError::AcceptancePanicked`] if a custom
    /// acceptance callback panics.
    fn accept_and_enqueue_reserved_job_with_guard(
        &self,
        mut job: PoolJob,
        reserved_transaction: Option<AcceptedJobGuard<'_>>,
    ) -> Result<(), PoolJobSubmissionError> {
        let entry = job.take_queue_entry();
        if let Some(entry) = &entry {
            crate::pool_job::begin_acceptance(&mut entry.lock(), crate::internal::sync::current_thread_id());
        }
        let mut transaction = reserved_transaction.unwrap_or_else(|| AcceptedJobGuard::new(self, entry.clone()));
        #[cfg(test)]
        self.notification_control
            .pause(crate::tests::notification_control::Phase::AcceptanceEntered);
        if job.accept().is_err() {
            drop(transaction);
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
            if let Err(cancelled_job) = crate::pool_job::publish_accepted_job(&mut state, job) {
                self.begin_cancel_queued_job();
                transaction.commit();
                drop(state);
                drop(queue);
                cancelled_job.cancel();
                self.finish_cancelled_job();
                crate::pool_job::finish_cancellation(&mut entry.lock());
                entry.changed.notify_all();
                return Ok(());
            }
            drop(state);
            queue.push_back(entry);
        } else {
            queue.push_back(Arc::new(QueuedJob::new(job)));
        }
        drop(queue);
        transaction.commit();
        #[cfg(test)]
        self.notification_control
            .signal(crate::tests::notification_control::Phase::JobPublished);
        self.wake_one_idle_worker();
        Ok(())
    }

    /// Removes `entry` and cancels its callbacks once, returning whether it
    /// won. Queue removal and accounting occur before invoking user code
    /// outside locks.
    ///
    /// # Parameters
    ///
    /// * `entry` - Queued job entry to remove and cancel.
    ///
    /// # Returns
    ///
    /// `true` if this call removed the queued entry and performed its
    /// cancellation callbacks; otherwise, `false`.
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

    /// Submits a typed callable while retaining it until admission succeeds.
    ///
    /// # Type Parameters
    ///
    /// * `C` - Callable submitted to the pool.
    /// * `R` - Successful result produced by the callable.
    /// * `E` - Error produced by the callable.
    ///
    /// # Parameters
    ///
    /// * `task` - Callable to submit.
    ///
    /// # Returns
    ///
    /// A task handle when accepted, or the unchanged callable and rejection
    /// reason when admission fails.
    pub(crate) fn try_submit_callable<C, R, E>(
        self: &Arc<Self>,
        task: C,
    ) -> Result<qubit_executor::TaskHandle<R, E>, RejectedTask<C>>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let _guard = match self.begin_submit() {
            Ok(guard) => guard,
            Err(reason) => return Err(RejectedTask::new(reason, task)),
        };
        if let Err(error) = self.reserve_submission_with_state_lock() {
            return Err(RejectedTask::new(error, task));
        }
        let transaction = AcceptedJobGuard::new(self, None);
        let (handle, completion) = TaskEndpointPair::new().into_parts();
        let job = PoolJob::from_task(task, completion);
        if let Err(error) = self.accept_and_enqueue_reserved_job_with_guard(job, Some(transaction)) {
            panic!("typed callable acceptance unexpectedly failed: {error}");
        }
        Ok(handle)
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
    /// Queued submissions can add at most one pending wake token per call.
    /// The monitor notification is broadcast because workers and lifecycle
    /// waiters share it: a single notification could wake an older waiter
    /// whose predicate is still false and leave the worker asleep. The token
    /// does not guarantee that only one worker wakes.
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
            return Err(error.into());
        }
        self.accept_and_enqueue_reserved_job(job)
    }

    /// Reserves admission capacity under the state monitor without taking job
    /// ownership, so rejected captures are always destroyed outside the lock.
    /// Returns Shutdown, Saturated, or WorkerSpawnFailed if reservation fails.
    fn reserve_submission_with_state_lock(self: &Arc<Self>) -> Result<(), SubmissionError> {
        let mut state = self.lock_state();
        if state.lifecycle != ExecutorServiceLifecycle::Running {
            return Err(SubmissionError::Shutdown);
        }
        if state.live_workers < state.core_pool_size {
            let worker = self.reserve_worker_locked(&mut state);
            self.spawn_reserved_worker_locked(worker)?;
            self.reserve_worker_handoff_slot();
            return Ok(());
        }
        if state.live_workers == 0 {
            let worker = self.reserve_worker_locked(&mut state);
            self.spawn_reserved_worker_locked(worker)?;
            self.reserve_worker_handoff_slot();
            return Ok(());
        }
        if self.reserve_queue_slot() {
            return Ok(());
        }
        if state.live_workers < state.maximum_pool_size {
            let worker = self.reserve_worker_locked(&mut state);
            self.spawn_reserved_worker_locked(worker)?;
            self.reserve_worker_handoff_slot();
            Ok(())
        } else {
            Err(SubmissionError::Saturated)
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
        self.spawn_reserved_worker_locked(worker)?;
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
    fn reserve_worker_locked<'a>(&'a self, state: &'a mut ThreadPoolState) -> ReservedWorker<'a> {
        let index = state.next_worker_index;
        state.next_worker_index += 1;
        state.live_workers += 1;
        let reservation = ReservedWorker::new(self, state, index);
        #[cfg(test)]
        if self.panic_after_worker_reservation.swap(false, Ordering::AcqRel) {
            panic!("injected panic after worker reservation");
        }
        reservation
    }

    /// Spawns a previously reserved worker while holding the state lock.
    ///
    /// # Parameters
    ///
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
    fn spawn_reserved_worker_locked(self: &Arc<Self>, mut worker: ReservedWorker<'_>) -> Result<(), SubmissionError> {
        #[cfg(test)]
        if self.fail_next_worker_spawn.swap(false, Ordering::AcqRel) {
            return Err(SubmissionError::WorkerSpawnFailed {
                source: Arc::new(std::io::Error::other("injected worker spawn failure")),
            });
        }
        let index = worker.index;
        let worker_inner = Arc::clone(self);
        let mut builder = thread::Builder::new().name(format!("{}-{index}", self.thread_name_prefix));
        if let Some(stack_size) = self.stack_size {
            builder = builder.stack_size(stack_size);
        }
        match builder.spawn(move || {
            ThreadPoolWorker::run(worker_inner, index);
        }) {
            Ok(_) => {
                worker.commit_spawn();
                Ok(())
            }
            Err(source) => Err(SubmissionError::WorkerSpawnFailed {
                source: Arc::new(source),
            }),
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

    /// Adds one idle-worker wake token if no existing token covers an idle
    /// worker, then broadcasts to monitor waiters.
    ///
    /// Pending wake tokens close the lost-notification window between a worker
    /// marking itself idle and actually parking on the condition variable. The
    /// broadcast is needed because lifecycle waiters share the monitor; a
    /// single notification could select a waiter whose predicate remains false.
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
            // Workers and lifecycle waiters share this monitor; a single wake
            // could select an older waiter whose predicate remains false.
            self.lock_state().notify_all();
        }
    }

    /// Returns whether an idle-worker wakeup has been requested.
    ///
    /// # Returns
    ///
    /// `true` when a worker should retry taking work instead of parking.
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    ///
    /// # Parameters
    ///
    /// * `timeout` - Maximum duration to wait for the terminal state.
    ///
    /// # Returns
    ///
    /// `true` if the pool is terminated when observed, or `false` if the
    /// timeout expires while the pool remains active.
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
            #[cfg(test)]
            self.notification_control
                .pause(crate::tests::notification_control::Phase::IdleWaitBoundary);
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
    #[must_use]
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
        #[cfg(feature = "async-wait")]
        self.notify_capacity_changed();
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
        #[cfg(feature = "async-wait")]
        self.notify_capacity_changed();
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
    /// `true` when shutdown has started and no live workers, exiting workers,
    /// or jobs remain active.
    fn is_terminated_locked(&self, state: &ThreadPoolState) -> bool {
        state.lifecycle != ExecutorServiceLifecycle::Running
            && state.live_workers == 0
            && state.exiting_workers == 0
            && self.is_idle_snapshot()
    }

    /// Completes worker exit bookkeeping after its stop hook has returned.
    ///
    /// # Panics
    ///
    /// Panics if no worker is currently recorded as exiting.
    pub(crate) fn finish_worker_exit(&self) {
        let mut state = self.lock_state();
        state.exiting_workers = state
            .exiting_workers
            .checked_sub(1)
            .expect("thread pool exiting worker counter underflow");
        self.notify_if_terminated(&state);
        state.notify_all();
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
    use std::sync::atomic::AtomicUsize;
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
    use super::internal::AcceptedJobGuard;
    use crate::PoolJob;
    use crate::PoolJobSubmissionError;
    use crate::pool_job::QueuedJobState;

    #[test]
    fn test_acceptance_guard_unwind_releases_slot_and_rejects_entry() {
        let inner = Arc::new(ThreadPoolInner::new(
            ThreadPoolConfig {
                core_pool_size: 1,
                maximum_pool_size: 1,
                queue_capacity: Some(1),
                thread_name_prefix: "accept-guard-test".to_owned(),
                stack_size: None,
                keep_alive: Duration::from_secs(1),
                allow_core_thread_timeout: false,
            },
            ThreadPoolHooks::default(),
        ));
        assert!(inner.reserve_queue_slot());
        let entry = Arc::new(crate::pool_job::QueuedJob::prepared(&inner));
        crate::pool_job::begin_acceptance(&mut entry.lock(), crate::internal::sync::current_thread_id());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let inner = Arc::clone(&inner);
            let entry = Arc::clone(&entry);
            move || {
                let _guard = AcceptedJobGuard::new(&inner, Some(entry));
                panic!("injected unwind during acceptance transaction");
            }
        }));
        assert!(result.is_err());
        assert_eq!(inner.queued_count(), 0);
        assert!(matches!(*entry.lock(), QueuedJobState::Rejected));
        assert!(inner.reserve_queue_slot(), "unwound transaction must release its slot");
        inner.release_queue_slot();
        inner.shutdown();
        assert!(inner.wait_for_termination_timeout(Duration::from_secs(2)));
    }

    #[test]
    fn test_recoverable_worker_spawn_failure_returns_callable_and_rolls_back_reservation() {
        struct Capture(Arc<AtomicUsize>);
        impl Drop for Capture {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let inner = Arc::new(ThreadPoolInner::new(
            ThreadPoolConfig {
                core_pool_size: 1,
                maximum_pool_size: 1,
                queue_capacity: Some(1),
                thread_name_prefix: "recoverable-spawn-failure".to_owned(),
                stack_size: None,
                keep_alive: Duration::from_secs(1),
                allow_core_thread_timeout: false,
            },
            ThreadPoolHooks::default(),
        ));
        inner.fail_next_worker_spawn.store(true, Ordering::Release);
        let drops = Arc::new(AtomicUsize::new(0));
        let capture = Capture(Arc::clone(&drops));
        let rejected = match inner.try_submit_callable(move || {
            let _capture = &capture;
            Ok::<(), std::io::Error>(())
        }) {
            Ok(_) => panic!("injected worker spawn failure should reject"),
            Err(rejected) => rejected,
        };
        assert!(matches!(rejected.reason(), SubmissionError::WorkerSpawnFailed { .. }));
        assert_eq!(0, drops.load(Ordering::SeqCst));
        let (_, callable) = rejected.into_parts();
        drop(callable);
        assert_eq!(1, drops.load(Ordering::SeqCst));
        assert_eq!(0, inner.read_state(|state| state.live_workers));
        assert_eq!(0, inner.queued_count());
        assert!(
            inner.reserve_queue_slot(),
            "spawn failure must roll back its queue slot"
        );
        inner.release_queue_slot();
        inner.shutdown();
        assert!(inner.wait_for_termination_timeout(Duration::from_secs(2)));
    }

    struct DynamicWakeCleanup {
        inner: Arc<ThreadPoolInner>,
        release_senders: Vec<mpsc::Sender<()>>,
        threads: Vec<thread::JoinHandle<()>>,
        finished: bool,
    }

    impl DynamicWakeCleanup {
        fn new(inner: Arc<ThreadPoolInner>) -> Self {
            Self {
                inner,
                release_senders: Vec::new(),
                threads: Vec::new(),
                finished: false,
            }
        }

        fn release_on_cleanup(&mut self, sender: mpsc::Sender<()>) {
            self.release_senders.push(sender);
        }

        fn track(&mut self, handle: thread::JoinHandle<()>) {
            self.threads.push(handle);
        }

        fn finish(&mut self) -> bool {
            if self.finished {
                return true;
            }
            self.finished = true;
            let inner = Arc::clone(&self.inner);
            let release_senders = std::mem::take(&mut self.release_senders);
            let handles = std::mem::take(&mut self.threads);
            let (finished_tx, finished_rx) = mpsc::channel();
            thread::spawn(move || {
                for sender in release_senders {
                    let _ = sender.send(());
                }
                inner.shutdown();
                let terminated = inner.wait_for_termination_timeout(Duration::from_secs(2));
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut all_joined = true;
                for handle in handles {
                    let (joined_tx, joined_rx) = mpsc::channel();
                    thread::spawn(move || {
                        let _ = joined_tx.send(handle.join().is_ok());
                    });
                    if !matches!(
                        joined_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
                        Ok(true)
                    ) {
                        all_joined = false;
                    }
                }
                let _ = finished_tx.send(terminated && all_joined);
            });
            matches!(finished_rx.recv_timeout(Duration::from_secs(6)), Ok(true))
        }
    }

    impl Drop for DynamicWakeCleanup {
        fn drop(&mut self) {
            let _ = self.finish();
        }
    }

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

    fn wait_for_cancel_reservation(entry: &crate::pool_job::QueuedJob) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let requested = matches!(
                &*entry.lock(),
                QueuedJobState::Accepting {
                    cancel_requested: true,
                    ..
                }
            );
            if requested {
                return;
            }
            assert!(Instant::now() < deadline, "cancellation reservation was not observed");
            thread::yield_now();
        }
    }

    #[test]
    fn test_ticket_cancel_reservation_precedes_accept_success_and_duplicate() {
        let inner = Arc::new(ThreadPoolInner::new(
            ThreadPoolConfig {
                core_pool_size: 1,
                maximum_pool_size: 1,
                queue_capacity: Some(1),
                thread_name_prefix: String::from("accept-cancel-success"),
                stack_size: None,
                keep_alive: Duration::from_secs(1),
                allow_core_thread_timeout: false,
            },
            ThreadPoolHooks::default(),
        ));
        let (accept_started_tx, accept_started_rx) = mpsc::channel();
        let (release_accept_tx, release_accept_rx) = mpsc::channel();
        let (run_tx, run_rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        let (job, ticket) = PoolJob::prepare_cancellable(
            &inner,
            Box::new(move || {
                accept_started_tx.send(()).expect("accept starts");
                release_accept_rx.recv().expect("accept released");
            }),
            Box::new(move || run_tx.send(()).expect("run observed")),
            Box::new(move || cancel_tx.send(()).expect("cancel observed")),
        );
        let entry = Arc::clone(&ticket.entry);
        let submit_inner = Arc::clone(&inner);
        let (submit_tx, submit_rx) = mpsc::channel();
        let submit = thread::spawn(move || submit_tx.send(submit_inner.submit(job)).expect("submit result"));
        accept_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("accept entered");
        let first_ticket = ticket.clone();
        let (first_tx, first_rx) = mpsc::channel();
        let first = thread::spawn(move || first_tx.send(first_ticket.cancel_queued()).expect("first result"));
        wait_for_cancel_reservation(&entry);
        assert!(!ticket.cancel_queued(), "duplicate cancellation must lose immediately");
        release_accept_tx.send(()).expect("release accept");
        assert!(
            first_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("first cancellation result")
        );
        submit_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("submit finishes")
            .expect("submit accepted");
        submit.join().expect("submit thread joins");
        first.join().expect("cancel thread joins");
        cancel_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("cancel runs once");
        assert!(run_rx.try_recv().is_err());
        inner.shutdown();
        assert!(inner.wait_for_termination_timeout(Duration::from_secs(2)));
        let stats = inner.stats();
        assert_eq!(stats.completed_tasks, 0);
        assert_eq!(stats.cancelled_tasks, 1);
        assert_eq!(stats.submitted_tasks, 1);
    }

    #[test]
    fn test_ticket_cancel_reservation_precedes_accept_panic() {
        let inner = Arc::new(ThreadPoolInner::new(
            ThreadPoolConfig {
                core_pool_size: 1,
                maximum_pool_size: 1,
                queue_capacity: Some(1),
                thread_name_prefix: String::from("accept-cancel-panic"),
                stack_size: None,
                keep_alive: Duration::from_secs(1),
                allow_core_thread_timeout: false,
            },
            ThreadPoolHooks::default(),
        ));
        let (accept_started_tx, accept_started_rx) = mpsc::channel();
        let (release_accept_tx, release_accept_rx) = mpsc::channel();
        let (job, ticket) = PoolJob::prepare_cancellable(
            &inner,
            Box::new(move || {
                accept_started_tx.send(()).expect("accept starts");
                release_accept_rx.recv().expect("accept released");
                panic!("acceptance fails after release");
            }),
            Box::new(|| panic!("rejected job must not run")),
            Box::new(|| panic!("rejected job must not cancel")),
        );
        let entry = Arc::clone(&ticket.entry);
        let submit_inner = Arc::clone(&inner);
        let (submit_tx, submit_rx) = mpsc::channel();
        let submit = thread::spawn(move || submit_tx.send(submit_inner.submit(job)).expect("submit result"));
        accept_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("accept entered");
        let (cancel_tx, cancel_rx) = mpsc::channel();
        let cancel_ticket = ticket.clone();
        let cancel = thread::spawn(move || cancel_tx.send(cancel_ticket.cancel_queued()).expect("cancel result"));
        wait_for_cancel_reservation(&entry);
        assert!(!ticket.cancel_queued(), "duplicate cancellation must lose immediately");
        release_accept_tx.send(()).expect("release accept");
        assert!(
            !cancel_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("cancellation result")
        );
        assert_eq!(
            submit_rx.recv_timeout(Duration::from_secs(2)).expect("submit finishes"),
            Err(PoolJobSubmissionError::AcceptancePanicked)
        );
        submit.join().expect("submit thread joins");
        cancel.join().expect("cancel thread joins");
        inner.shutdown();
        assert!(inner.wait_for_termination_timeout(Duration::from_secs(2)));
        let stats = inner.stats();
        assert_eq!(stats.completed_tasks, 0);
        assert_eq!(stats.cancelled_tasks, 0);
        assert_eq!(stats.submitted_tasks, 0);
    }

    #[test]
    fn test_submit_with_state_lock_rejects_stopping_state() {
        let inner = Arc::new(ThreadPoolInner::new(
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
    fn test_dynamic_worker_wake_with_older_termination_waiter() {
        let inner = Arc::new(ThreadPoolInner::new(
            ThreadPoolConfig {
                core_pool_size: 2,
                maximum_pool_size: 2,
                queue_capacity: None,
                thread_name_prefix: String::from("older-termination-waiter"),
                stack_size: None,
                keep_alive: Duration::from_secs(60),
                allow_core_thread_timeout: false,
            },
            ThreadPoolHooks::default(),
        ));
        let mut cleanup = DynamicWakeCleanup::new(Arc::clone(&inner));
        let (registered_tx, registered_rx) = mpsc::channel();
        let (terminated_tx, terminated_rx) = mpsc::channel();
        let waiter_inner = Arc::clone(&inner);
        cleanup.track(thread::spawn(move || {
            let mut state = waiter_inner.lock_state();
            registered_tx
                .send(())
                .expect("termination waiter should announce its predicate check");
            while !waiter_inner.is_terminated_locked(&state) {
                state.wait();
            }
            terminated_tx
                .send(())
                .expect("termination waiter should report completion");
        }));
        registered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("termination waiter should enter under the monitor lock");
        let (lock_done_tx, lock_done_rx) = mpsc::channel();
        let lock_inner = Arc::clone(&inner);
        cleanup.track(thread::spawn(move || {
            drop(lock_inner.lock_state());
            lock_done_tx.send(()).expect("lock handshake should complete");
        }));
        lock_done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("termination waiter should register before the monitor is reacquired");

        let mut state = inner.lock_state();
        let first_reservation = inner.reserve_worker_locked(&mut state);
        inner
            .spawn_reserved_worker_locked(first_reservation)
            .expect("first dynamic worker should spawn");
        let second_reservation = inner.reserve_worker_locked(&mut state);
        inner
            .spawn_reserved_worker_locked(second_reservation)
            .expect("second dynamic worker should spawn");
        drop(state);
        let idle_deadline = Instant::now() + Duration::from_secs(2);
        while inner.idle_worker_count.load(Ordering::Acquire) != 2 && Instant::now() < idle_deadline {
            thread::yield_now();
        }
        assert_eq!(
            inner.idle_worker_count.load(Ordering::Acquire),
            2,
            "both workers should reach idle wait"
        );
        let (worker_lock_tx, worker_lock_rx) = mpsc::channel();
        let worker_lock_inner = Arc::clone(&inner);
        cleanup.track(thread::spawn(move || {
            drop(worker_lock_inner.lock_state());
            worker_lock_tx.send(()).expect("worker lock handshake should complete");
        }));
        worker_lock_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("both workers should register their waits before the monitor is reacquired");

        let (task_tx, task_rx) = mpsc::channel();
        let (task_release_tx, task_release_rx) = mpsc::channel();
        cleanup.release_on_cleanup(task_release_tx.clone());
        let task_count = Arc::new(AtomicUsize::new(0));
        let task_count_for_job = Arc::clone(&task_count);
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (accept_release_tx, accept_release_rx) = mpsc::channel();
        cleanup.release_on_cleanup(accept_release_tx.clone());
        let job = PoolJob::with_accept(
            Box::new(move || {
                accepted_tx.send(()).expect("acceptance should be observed");
                accept_release_rx
                    .recv()
                    .expect("acceptance callback should be released");
            }),
            Box::new(move || {
                task_count_for_job.fetch_add(1, Ordering::AcqRel);
                task_tx.send(()).expect("task should report execution");
                task_release_rx
                    .recv()
                    .expect("running task should be released after the spare worker reparks");
            }),
            Box::new(|| {}),
        );
        let submit_inner = Arc::clone(&inner);
        let (submitted_tx, submitted_rx) = mpsc::channel();
        cleanup.track(thread::spawn(move || {
            submitted_tx
                .send(submit_inner.submit(job))
                .expect("submit result should be reported");
        }));
        accepted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("submitter should reserve admission before acceptance");
        let monitor_gate = inner.lock_state();
        accept_release_tx
            .send(())
            .expect("acceptance callback should be released after locking the monitor");
        let token_deadline = Instant::now() + Duration::from_secs(2);
        while inner.pending_worker_wakes.load(Ordering::Acquire) != 1 && Instant::now() < token_deadline {
            thread::yield_now();
        }
        assert_eq!(
            inner.pending_worker_wakes.load(Ordering::Acquire),
            1,
            "one queued task should publish exactly one worker wake token"
        );
        drop(monitor_gate);
        submitted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("submit should finish after the monitor gate opens")
            .expect("task should be accepted while admission is open");
        task_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("broadcast should wake a worker to run the task");
        let spare_worker_snapshot = || {
            inner.read_state(|state| {
                (
                    state.idle_workers,
                    inner.idle_worker_count.load(Ordering::Acquire),
                    inner.pending_worker_wakes.load(Ordering::Acquire),
                )
            })
        };
        let spare_idle_deadline = Instant::now() + Duration::from_secs(2);
        while spare_worker_snapshot() != (1, 1, 0) && Instant::now() < spare_idle_deadline {
            thread::yield_now();
        }
        assert_eq!(spare_worker_snapshot(), (1, 1, 0), "spare worker should be idle again");
        let (spare_reparked_tx, spare_reparked_rx) = mpsc::channel();
        let spare_reparked_inner = Arc::clone(&inner);
        cleanup.track(thread::spawn(move || {
            drop(spare_reparked_inner.lock_state());
            spare_reparked_tx
                .send(())
                .expect("spare worker repark handshake should complete");
        }));
        spare_reparked_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("spare worker should recheck the queue and return to waiting");
        task_release_tx
            .send(())
            .expect("running task should be released after the spare worker reparks");
        let both_workers_snapshot = || {
            inner.read_state(|state| {
                (
                    state.idle_workers,
                    inner.idle_worker_count.load(Ordering::Acquire),
                    inner.pending_worker_wakes.load(Ordering::Acquire),
                )
            })
        };
        let idle_deadline = Instant::now() + Duration::from_secs(2);
        while both_workers_snapshot() != (2, 2, 0) && Instant::now() < idle_deadline {
            thread::yield_now();
        }
        assert_eq!(both_workers_snapshot(), (2, 2, 0), "both workers should be idle again");
        let (reparked_tx, reparked_rx) = mpsc::channel();
        let reparked_inner = Arc::clone(&inner);
        cleanup.track(thread::spawn(move || {
            drop(reparked_inner.lock_state());
            reparked_tx.send(()).expect("repark handshake should complete");
        }));
        reparked_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("both workers should recheck the queue and return to waiting");
        assert_eq!(
            task_count.load(Ordering::Acquire),
            1,
            "the task should execute exactly once"
        );
        assert!(
            task_rx.try_recv().is_err(),
            "no duplicate task execution should be reported"
        );
        assert!(
            terminated_rx.try_recv().is_err(),
            "termination waiter should remain blocked while the pool is running"
        );

        assert!(
            cleanup.finish(),
            "shutdown cleanup should terminate and join test threads"
        );
        terminated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("termination waiter should report completion");
    }

    #[test]
    fn test_worker_reservation_rolls_back_during_unwind() {
        let inner = Arc::new(ThreadPoolInner::new(
            ThreadPoolConfig {
                core_pool_size: 1,
                maximum_pool_size: 1,
                queue_capacity: Some(1),
                thread_name_prefix: "reservation-unwind".to_owned(),
                stack_size: None,
                keep_alive: Duration::from_secs(60),
                allow_core_thread_timeout: false,
            },
            ThreadPoolHooks::default(),
        ));
        inner.panic_after_worker_reservation.store(true, Ordering::Release);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut state = inner.lock_state();
            let _reservation = inner.reserve_worker_locked(&mut state);
        }));

        assert!(result.is_err(), "test injection should unwind after reserving");
        assert_eq!(inner.live_worker_count.load(Ordering::Acquire), 0);
        assert_eq!(inner.read_state(|state| state.live_workers), 0);
        assert_eq!(inner.queued_count(), 0, "reservation must not leak a queue slot");
        inner.shutdown();
        assert!(inner.wait_for_termination_timeout(Duration::from_secs(2)));
    }
}
