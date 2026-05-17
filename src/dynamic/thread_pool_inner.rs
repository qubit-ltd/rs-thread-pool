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
            AtomicBool,
            AtomicUsize,
            Ordering,
        },
        mpsc,
    },
    thread,
    time::Duration,
};

use crossbeam_deque::{
    Injector,
    Steal,
};
use qubit_executor::service::{
    ExecutorServiceLifecycle,
    StopReport,
    SubmissionError,
};
use qubit_lock::{
    ParkingLotMonitor,
    ParkingLotMonitorGuard,
};

use super::thread_pool_config::ThreadPoolConfig;
use super::thread_pool_state::ThreadPoolState;
use super::thread_pool_worker::ThreadPoolWorker;
use crate::{
    ExecutorServiceBuilderError,
    PoolJob,
    ThreadPoolHooks,
    ThreadPoolStats,
};

/// Submit guard that leaves in-flight accounting on drop.
struct ThreadPoolSubmitGuard<'a> {
    /// Pool whose in-flight counter was entered.
    inner: &'a ThreadPoolInner,
}

impl Drop for ThreadPoolSubmitGuard<'_> {
    /// Leaves submit accounting and wakes waiters if this was the last submitter.
    fn drop(&mut self) {
        let previous = self
            .inner
            .inflight_submissions
            .fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "thread pool submit counter underflow");
        if previous == 1 && (self.inner.has_submit_waiters() || self.inner.has_idle_waiters()) {
            self.inner.notify_waiters_after_atomic_change();
        }
    }
}

/// Shared state for a thread pool.
pub(crate) struct ThreadPoolInner {
    /// Lifecycle and worker state protected by a monitor.
    state_monitor: ParkingLotMonitor<ThreadPoolState>,
    /// Admission gate used by submitters.
    accepting: AtomicBool,
    /// Whether immediate shutdown has requested workers to stop taking jobs.
    stop_now: AtomicBool,
    /// Successfully spawned workers that have not exited yet.
    live_worker_count: AtomicUsize,
    /// Current core size mirrored from the locked state for submit fast paths.
    core_pool_size: AtomicUsize,
    /// Submit calls that have passed the first admission check.
    inflight_submissions: AtomicUsize,
    /// Number of workers currently blocked or about to block waiting for work.
    idle_worker_count: AtomicUsize,
    /// Number of idle-worker wakeups already requested but not yet consumed.
    pending_worker_wakes: AtomicUsize,
    /// Number of callers waiting for in-flight submitters to leave admission.
    submit_waiter_count: AtomicUsize,
    /// Number of callers waiting for accepted work to become idle.
    idle_waiter_count: AtomicUsize,
    /// Global FIFO-ish submission queue for worker consumption.
    global_queue: Injector<PoolJob>,
    /// Optional maximum number of queued jobs.
    queue_capacity: Option<usize>,
    /// Accepted work not yet started or fully cancelled.
    queue_slot_count: AtomicUsize,
    /// Published queued jobs not yet started or claimed for cancellation.
    queued_task_count: AtomicUsize,
    /// Jobs currently held by workers.
    running_task_count: AtomicUsize,
    /// Queued-job cancellation callbacks currently running.
    cancelling_task_count: AtomicUsize,
    /// Total number of jobs accepted since pool creation.
    submitted_task_count: AtomicUsize,
    /// Total number of worker-held jobs that have completed.
    completed_task_count: AtomicUsize,
    /// Total number of queued jobs whose cancellation callback completed.
    cancelled_task_count: AtomicUsize,
    /// Prefix used for naming newly spawned workers.
    thread_name_prefix: String,
    /// Optional stack size in bytes for newly spawned workers.
    stack_size: Option<usize>,
    /// Worker and task lifecycle hooks.
    hooks: ThreadPoolHooks,
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
        let core_pool_size = config.core_pool_size;
        Self {
            state_monitor: ParkingLotMonitor::new(ThreadPoolState::new(config)),
            accepting: AtomicBool::new(true),
            stop_now: AtomicBool::new(false),
            live_worker_count: AtomicUsize::new(0),
            core_pool_size: AtomicUsize::new(core_pool_size),
            inflight_submissions: AtomicUsize::new(0),
            idle_worker_count: AtomicUsize::new(0),
            pending_worker_wakes: AtomicUsize::new(0),
            submit_waiter_count: AtomicUsize::new(0),
            idle_waiter_count: AtomicUsize::new(0),
            global_queue: Injector::new(),
            queue_capacity,
            queue_slot_count: AtomicUsize::new(0),
            queued_task_count: AtomicUsize::new(0),
            running_task_count: AtomicUsize::new(0),
            cancelling_task_count: AtomicUsize::new(0),
            submitted_task_count: AtomicUsize::new(0),
            completed_task_count: AtomicUsize::new(0),
            cancelled_task_count: AtomicUsize::new(0),
            thread_name_prefix,
            stack_size,
            hooks,
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
        self.queued_task_count.load(Ordering::Acquire)
    }

    /// Returns the running work count.
    ///
    /// # Returns
    ///
    /// Number of jobs currently held by workers.
    #[inline]
    pub(crate) fn running_count(&self) -> usize {
        self.running_task_count.load(Ordering::Acquire)
    }

    /// Returns the number of submit calls currently inside admission.
    ///
    /// # Returns
    ///
    /// Number of submitters that may still publish, spawn, or roll back work.
    #[inline]
    fn inflight_count(&self) -> usize {
        self.inflight_submissions.load(Ordering::Acquire)
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
        self.state_monitor.read(f)
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
        self.state_monitor.write(f)
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
        self.inflight_submissions.fetch_add(1, Ordering::Release);
        if self.accepting.load(Ordering::Acquire) {
            Ok(ThreadPoolSubmitGuard { inner: self })
        } else {
            let previous = self.inflight_submissions.fetch_sub(1, Ordering::Release);
            debug_assert!(previous > 0, "thread pool submit counter underflow");
            if previous == 1 && self.has_submit_waiters() {
                self.notify_waiters_after_atomic_change();
            }
            Err(SubmissionError::Shutdown)
        }
    }

    /// Returns whether any caller is waiting for submit admission to drain.
    ///
    /// # Returns
    ///
    /// `true` when the last in-flight submitter should wake submit waiters.
    fn has_submit_waiters(&self) -> bool {
        self.submit_waiter_count.load(Ordering::Acquire) > 0
    }

    /// Returns whether any caller is waiting for accepted work to drain.
    ///
    /// # Returns
    ///
    /// `true` when the last in-flight submitter should wake idle waiters.
    fn has_idle_waiters(&self) -> bool {
        self.idle_waiter_count.load(Ordering::Acquire) > 0
    }

    /// Attempts to reserve one queued-work slot.
    ///
    /// # Returns
    ///
    /// `true` when a slot was reserved.
    fn reserve_queue_slot(&self) -> bool {
        if let Some(capacity) = self.queue_capacity {
            return self
                .queue_slot_count
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    (current < capacity).then_some(current + 1)
                })
                .is_ok();
        }
        self.queue_slot_count.fetch_add(1, Ordering::Release);
        true
    }

    /// Releases one reserved queued-work slot that never became runnable.
    fn release_queue_slot(&self) {
        let previous = self.queue_slot_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "thread pool queue slot counter underflow");
    }

    /// Returns whether a queued submit can skip the pool-state monitor.
    ///
    /// # Returns
    ///
    /// `true` when a successfully spawned worker exists and the core size is
    /// already satisfied.
    fn can_submit_to_queue_without_state_lock(&self) -> bool {
        let live_workers = self.live_worker_count.load(Ordering::Acquire);
        live_workers > 0 && live_workers >= self.core_pool_size.load(Ordering::Acquire)
    }

    /// Attempts the lock-free queued submit path.
    ///
    /// # Parameters
    ///
    /// * `job` - Job to accept and queue if the fast path is usable.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the job was handled, or `Err(job)` when the caller must use
    /// the locked slow path.
    fn try_submit_to_queue_without_state_lock(&self, job: PoolJob) -> Result<(), PoolJob> {
        if !self.can_submit_to_queue_without_state_lock() || !self.reserve_queue_slot() {
            return Err(job);
        }
        self.accept_and_enqueue_reserved_job(job);
        Ok(())
    }

    /// Accepts a job whose queue slot has already been reserved and publishes it.
    ///
    /// This method may run custom acceptance callbacks and therefore must not be
    /// called while holding the pool-state monitor.
    ///
    /// # Parameters
    ///
    /// * `job` - Job with one reserved queue slot.
    fn accept_and_enqueue_reserved_job(&self, job: PoolJob) {
        self.submitted_task_count.fetch_add(1, Ordering::Release);
        if !job.accept() {
            self.release_queue_slot();
            self.completed_task_count.fetch_add(1, Ordering::Release);
            self.notify_waiters_after_atomic_change();
            return;
        }
        self.queued_task_count.fetch_add(1, Ordering::Release);
        self.global_queue.push(job);
        self.wake_one_idle_worker();
    }

    /// Submits a job into the queue.
    ///
    /// # Overall logic
    ///
    /// This method first tries the hot queued path without taking the pool-state
    /// monitor. It falls back to locked dynamic admission only when the pool must
    /// spawn, grow, reject saturation, or observe a lifecycle transition:
    ///
    /// 1. Reject immediately if the lifecycle is not running.
    /// 2. If live workers are below the core size, spawn a worker and hand the
    ///    job to it directly (no queue hop).
    /// 3. Otherwise, try enqueuing the job if the queue is not saturated.
    /// 4. If the queue is saturated but live workers are still below maximum,
    ///    spawn a non-core worker with the job as its first task.
    /// 5. Otherwise reject as saturated.
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
    /// worker cannot be created.
    pub(crate) fn submit(self: &Arc<Self>, job: PoolJob) -> Result<(), SubmissionError> {
        let _guard = self.begin_submit()?;
        let job = match self.try_submit_to_queue_without_state_lock(job) {
            Ok(()) => return Ok(()),
            Err(job) => job,
        };
        self.submit_with_state_lock(job)
    }

    /// Submits a job through the locked dynamic admission path.
    ///
    /// # Parameters
    ///
    /// * `job` - Job that could not use the queue-only fast path.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the job is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`], [`SubmissionError::Saturated`], or
    /// [`SubmissionError::WorkerSpawnFailed`] according to the dynamic admission
    /// state observed under the monitor.
    fn submit_with_state_lock(self: &Arc<Self>, job: PoolJob) -> Result<(), SubmissionError> {
        let mut state = self.lock_state();
        debug_assert_eq!(state.lifecycle, ExecutorServiceLifecycle::Running);
        if state.live_workers < state.core_pool_size {
            let worker = self.reserve_worker_locked(&mut state);
            self.spawn_reserved_worker_with_initial_job_locked(&mut state, worker, job)?;
            return Ok(());
        }
        if state.live_workers == 0 {
            let worker = self.reserve_worker_locked(&mut state);
            self.spawn_reserved_worker_with_initial_job_locked(&mut state, worker, job)?;
            return Ok(());
        }
        if self.reserve_queue_slot() {
            drop(state);
            self.accept_and_enqueue_reserved_job(job);
            return Ok(());
        }
        if state.live_workers < state.maximum_pool_size {
            let worker = self.reserve_worker_locked(&mut state);
            self.spawn_reserved_worker_with_initial_job_locked(&mut state, worker, job)?;
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
        let mut builder =
            thread::Builder::new().name(format!("{}-{index}", self.thread_name_prefix));
        if let Some(stack_size) = self.stack_size {
            builder = builder.stack_size(stack_size);
        }
        match builder.spawn(move || {
            ThreadPoolWorker::run(worker_inner, index, None);
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

    /// Spawns a reserved worker with one already assigned initial job.
    ///
    /// The caller must hold the pool state monitor. A start gate keeps the worker
    /// from running the initial job until this method has observed a successful
    /// spawn and updated task counters. If the OS thread cannot be created, the
    /// job is dropped without crossing the acceptance boundary.
    ///
    /// # Parameters
    ///
    /// * `state` - Locked pool state used for counter updates and rollback.
    /// * `worker` - Worker reservation created while holding the state lock.
    /// * `job` - Initial job assigned directly to the new worker.
    ///
    /// # Returns
    ///
    /// `Ok(())` after the worker has been spawned and released to run its first
    /// job.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::WorkerSpawnFailed`] if the worker thread cannot
    /// be created.
    fn spawn_reserved_worker_with_initial_job_locked(
        self: &Arc<Self>,
        state: &mut ThreadPoolState,
        worker: ReservedWorker,
        job: PoolJob,
    ) -> Result<(), SubmissionError> {
        let ReservedWorker { index } = worker;
        let (start_sender, start_receiver) = mpsc::sync_channel(1);
        let worker_inner = Arc::clone(self);
        let mut builder =
            thread::Builder::new().name(format!("{}-{index}", self.thread_name_prefix));
        if let Some(stack_size) = self.stack_size {
            builder = builder.stack_size(stack_size);
        }
        match builder.spawn(move || {
            if start_receiver.recv().is_ok() {
                ThreadPoolWorker::run(worker_inner, index, Some(job));
            }
        }) {
            Ok(_) => {
                self.live_worker_count.fetch_add(1, Ordering::Release);
                self.submitted_task_count.fetch_add(1, Ordering::Release);
                self.running_task_count.fetch_add(1, Ordering::Release);
                let _released = start_sender.send(());
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
        if self.stop_now.load(Ordering::Acquire) {
            return None;
        }
        Self::steal_one(&self.global_queue).and_then(|job| self.accept_claimed_job(job))
    }

    /// Steals one job from a crossbeam injector with retry on contention.
    ///
    /// # Parameters
    ///
    /// * `queue` - Injector to steal from.
    ///
    /// # Returns
    ///
    /// `Some(job)` when the injector contains work, otherwise `None`.
    fn steal_one(queue: &Injector<PoolJob>) -> Option<PoolJob> {
        loop {
            match queue.steal() {
                Steal::Success(job) => return Some(job),
                Steal::Empty => return None,
                Steal::Retry => continue,
            }
        }
    }

    /// Accepts a claimed queued job or cancels it after immediate shutdown.
    ///
    /// # Parameters
    ///
    /// * `job` - Job claimed from the global queue.
    ///
    /// # Returns
    ///
    /// `Some(job)` when the job may run.
    fn accept_claimed_job(&self, job: PoolJob) -> Option<PoolJob> {
        if self.stop_now.load(Ordering::Acquire) {
            self.begin_cancel_queued_job();
            job.cancel();
            self.finish_cancelled_job();
            return None;
        }
        self.mark_queued_job_running();
        Some(job)
    }

    /// Marks one claimed queued job as running.
    fn mark_queued_job_running(&self) {
        let previous = self.queued_task_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "thread pool queued task counter underflow");
        let previous = self.queue_slot_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "thread pool queue slot counter underflow");
        self.running_task_count.fetch_add(1, Ordering::Release);
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
        let requested = self.pending_worker_wakes.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |pending_wakes| (pending_wakes < idle_workers).then_some(pending_wakes + 1),
        );
        if requested.is_ok() {
            let _state = self.lock_state();
            self.state_monitor.notify_one();
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
        let _ = self.pending_worker_wakes.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| current.checked_sub(1),
        );
    }

    /// Opens cancellation accounting for one queued job.
    fn begin_cancel_queued_job(&self) {
        let previous = self.queued_task_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "thread pool queued task counter underflow");
        self.cancelling_task_count.fetch_add(1, Ordering::Release);
    }

    /// Drains all jobs currently visible in the global queue.
    ///
    /// # Returns
    ///
    /// Drained queued jobs.
    fn drain_visible_queued_jobs(&self) -> Vec<PoolJob> {
        let mut jobs = Vec::new();
        while let Some(job) = Self::steal_one(&self.global_queue) {
            jobs.push(job);
        }
        jobs
    }

    /// Marks one running job as finished.
    pub(crate) fn finish_running_job(&self) {
        let previous = self.running_task_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "thread pool running task counter underflow");
        self.completed_task_count.fetch_add(1, Ordering::Release);
        if previous == 1 && self.queue_slot_count.load(Ordering::Acquire) == 0 {
            self.notify_waiters_after_atomic_change();
        }
    }

    /// Requests graceful shutdown.
    ///
    /// The pool rejects later submissions but lets queued work drain.
    pub(crate) fn shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        self.submit_waiter_count.fetch_add(1, Ordering::AcqRel);
        let mut state = self.lock_state();
        while self.inflight_count() > 0 {
            state = state.wait();
        }
        let previous = self.submit_waiter_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "thread pool submit waiter counter underflow");
        if state.lifecycle == ExecutorServiceLifecycle::Running {
            state.lifecycle = ExecutorServiceLifecycle::ShuttingDown;
        }
        self.state_monitor.notify_all();
        self.notify_if_terminated(&state);
    }

    /// Requests abrupt shutdown and cancels queued jobs.
    ///
    /// # Returns
    ///
    /// A report containing queued jobs cancelled and jobs running at the time
    /// of the request.
    pub(crate) fn stop(&self) -> StopReport {
        let cancelled_before_stop = self.cancelled_task_count.load(Ordering::Acquire);
        let cancelling_before_stop = self.cancelling_task_count.load(Ordering::Acquire);
        self.accepting.store(false, Ordering::Release);
        self.stop_now.store(true, Ordering::Release);
        let (jobs, queued, running) = {
            let mut state = self.lock_state();
            let is_new_stop = matches!(
                state.lifecycle,
                ExecutorServiceLifecycle::Running | ExecutorServiceLifecycle::ShuttingDown
            );
            if is_new_stop {
                state.lifecycle = ExecutorServiceLifecycle::Stopping;
            }
            if self.inflight_count() > 0 {
                self.submit_waiter_count.fetch_add(1, Ordering::AcqRel);
                while self.inflight_count() > 0 {
                    state = state.wait();
                }
                let previous = self.submit_waiter_count.fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "thread pool submit waiter counter underflow");
            }
            let running = self.running_count();
            let jobs = self.drain_visible_queued_jobs();
            let drained = jobs.len();
            for _ in 0..drained {
                self.begin_cancel_queued_job();
            }
            let cancelling_since_stop = self
                .cancelling_task_count
                .load(Ordering::Acquire)
                .saturating_sub(cancelling_before_stop);
            let cancelled_since_stop = self
                .cancelled_task_count
                .load(Ordering::Acquire)
                .saturating_sub(cancelled_before_stop);
            let queued = if is_new_stop {
                drained + cancelling_since_stop.saturating_sub(drained) + cancelled_since_stop
            } else {
                drained
            };
            self.state_monitor.notify_all();
            self.notify_if_terminated(&state);
            (jobs, queued, running)
        };
        for job in jobs {
            job.cancel();
            self.finish_cancelled_job();
        }
        self.state_monitor.notify_all();
        StopReport::new(queued, running, queued)
    }

    /// Marks one queued-job cancellation callback as completed.
    ///
    /// This method closes the queue slot only after the callback returns, so
    /// join and termination waiters cannot observe a cancelled job as fully
    /// inactive while user cancellation code is still running.
    fn finish_cancelled_job(&self) {
        let previous = self.cancelling_task_count.fetch_sub(1, Ordering::Release);
        debug_assert!(
            previous > 0,
            "thread pool cancelling task counter underflow"
        );
        let previous = self.queue_slot_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "thread pool queue slot counter underflow");
        self.cancelled_task_count.fetch_add(1, Ordering::Release);
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
            .wait_until(|state| self.is_terminated_locked(state), |_| ());
    }

    /// Blocks until all currently accepted work has completed.
    ///
    /// This method waits for queued and running tasks to drain, but it does not
    /// request shutdown and does not wait for worker threads to exit.
    pub(crate) fn wait_until_idle(&self) {
        self.idle_waiter_count.fetch_add(1, Ordering::AcqRel);
        let mut state = self.lock_state();
        while !self.is_idle_snapshot() {
            state = state.wait();
        }
        let previous = self.idle_waiter_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "thread pool idle waiter counter underflow");
    }

    /// Returns a point-in-time pool snapshot.
    ///
    /// # Returns
    ///
    /// A snapshot built while holding the pool state lock.
    pub(crate) fn stats(&self) -> ThreadPoolStats {
        let queued_tasks = self.queued_count();
        let running_tasks = self.running_count();
        let submitted_tasks = self.submitted_task_count.load(Ordering::Acquire);
        let completed_tasks = self.completed_task_count.load(Ordering::Acquire);
        let cancelled_tasks = self.cancelled_task_count.load(Ordering::Acquire);
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
                queued_tasks,
                running_tasks,
                submitted_tasks,
                completed_tasks,
                cancelled_tasks,
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
    /// Returns [`ExecutorServiceBuilderError::CorePoolSizeExceedsMaximum`] when the
    /// new core size is greater than the current maximum size.
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
        self.core_pool_size.store(core_pool_size, Ordering::Release);
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
    /// Returns [`ExecutorServiceBuilderError::ZeroMaximumPoolSize`] for zero, or
    /// [`ExecutorServiceBuilderError::CorePoolSizeExceedsMaximum`] when the current
    /// core size is greater than the new maximum size.
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
    /// Returns [`ExecutorServiceBuilderError::ZeroKeepAlive`] when the duration is
    /// zero.
    pub(crate) fn set_keep_alive(
        &self,
        keep_alive: Duration,
    ) -> Result<(), ExecutorServiceBuilderError> {
        if keep_alive.is_zero() {
            return Err(ExecutorServiceBuilderError::ZeroKeepAlive);
        }
        self.write_state(|state| state.keep_alive = keep_alive);
        self.state_monitor.notify_all();
        Ok(())
    }

    /// Updates whether idle core workers may time out.
    ///
    /// # Parameters
    ///
    /// * `allow` - Whether idle core workers may retire after keep-alive.
    pub(crate) fn allow_core_thread_timeout(&self, allow: bool) {
        self.write_state(|state| state.allow_core_thread_timeout = allow);
        self.state_monitor.notify_all();
    }

    /// Checks whether all accepted work has drained.
    ///
    /// # Returns
    ///
    /// `true` when no queued slot, running job, or cancellation callback remains.
    fn is_idle_snapshot(&self) -> bool {
        self.queue_slot_count.load(Ordering::Acquire) == 0
            && self.running_count() == 0
            && self.cancelling_task_count.load(Ordering::Acquire) == 0
            && self.inflight_count() == 0
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
        state.lifecycle != ExecutorServiceLifecycle::Running
            && state.live_workers == 0
            && self.is_idle_snapshot()
    }

    /// Notifies waiters after an atomic-only condition change.
    fn notify_waiters_after_atomic_change(&self) {
        let _state = self.lock_state();
        self.state_monitor.notify_all();
    }

    /// Notifies termination waiters when the state is terminal.
    ///
    /// # Parameters
    ///
    /// * `state` - Current pool state observed while holding the state lock.
    pub(crate) fn notify_if_terminated(&self, state: &ThreadPoolState) {
        if self.is_terminated_locked(state) {
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

/// Worker reservation created under the pool state lock before thread spawn.
struct ReservedWorker {
    /// Stable worker index assigned in pool state.
    index: usize,
}
