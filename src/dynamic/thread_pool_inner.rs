// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

mod internal;

use crossbeam_deque::Injector;
use crossbeam_deque::Steal;
use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_executor::service::StopReport;
use qubit_executor::service::SubmissionError;
use qubit_lock::ParkingLotMonitor;
use qubit_lock::ParkingLotMonitorGuard;

pub(super) use self::internal::InitialWorkerDecision;
pub(super) use self::internal::InitialWorkerJob;
pub(super) use self::internal::InitialWorkerStartup;
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
use crate::internal::AdmissionGate;

/// Shared state for a thread pool.
pub(crate) struct ThreadPoolInner {
    /// Lifecycle and worker state protected by a monitor.
    state_monitor: ParkingLotMonitor<ThreadPoolState>,
    /// Admission gate used by submitters.
    admission: AdmissionGate,
    /// Whether immediate shutdown has requested workers to stop taking jobs.
    stop_now: AtomicBool,
    /// Successfully spawned workers that have not exited yet.
    live_worker_count: AtomicUsize,
    /// Submit calls that have passed the first admission check.
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
        Self {
            state_monitor: ParkingLotMonitor::new(ThreadPoolState::new(config)),
            admission: AdmissionGate::new(),
            stop_now: AtomicBool::new(false),
            live_worker_count: AtomicUsize::new(0),
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
    pub(crate) fn inflight_count(&self) -> usize {
        self.admission.inflight_count()
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
        if self.admission.try_enter() {
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

    /// Accepts a job whose queue slot has already been reserved and publishes
    /// it.
    ///
    /// This method may run custom acceptance callbacks and therefore must not
    /// be called while holding the pool-state monitor.
    ///
    /// # Parameters
    ///
    /// * `job` - Job with one reserved queue slot.
    fn accept_and_enqueue_reserved_job(&self, job: PoolJob) -> Result<(), PoolJobSubmissionError> {
        if job.accept().is_err() {
            self.release_queue_slot();
            self.notify_waiters_after_atomic_change();
            return Err(PoolJobSubmissionError::AcceptancePanicked);
        }
        self.submitted_task_count.fetch_add(1, Ordering::Release);
        self.queued_task_count.fetch_add(1, Ordering::Release);
        self.global_queue.push(job);
        self.wake_one_idle_worker();
        Ok(())
    }

    /// Submits a job into the queue.
    ///
    /// # Overall logic
    ///
    /// This method performs dynamic admission while holding the pool-state
    /// monitor when it must inspect worker capacity or lifecycle state:
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
    pub(crate) fn submit(self: &Arc<Self>, job: PoolJob) -> Result<(), PoolJobSubmissionError> {
        let _guard = self.begin_submit()?;
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
    /// Returns [`SubmissionError::Shutdown`], [`SubmissionError::Saturated`],
    /// or [`SubmissionError::WorkerSpawnFailed`] according to the dynamic
    /// admission state observed under the monitor.
    fn submit_with_state_lock(
        self: &Arc<Self>,
        job: PoolJob,
    ) -> Result<(), PoolJobSubmissionError> {
        let mut state = self.lock_state();
        if state.lifecycle != ExecutorServiceLifecycle::Running {
            return Err(PoolJobSubmissionError::Rejected(SubmissionError::Shutdown));
        }
        if state.live_workers < state.core_pool_size {
            let worker = self.reserve_worker_locked(&mut state);
            let startup = self.spawn_reserved_worker_with_initial_job_locked(&mut state, worker)?;
            drop(state);
            return self.start_reserved_worker(startup, job);
        }
        if state.live_workers == 0 {
            let worker = self.reserve_worker_locked(&mut state);
            let startup = self.spawn_reserved_worker_with_initial_job_locked(&mut state, worker)?;
            drop(state);
            return self.start_reserved_worker(startup, job);
        }
        if self.reserve_queue_slot() {
            drop(state);
            return self.accept_and_enqueue_reserved_job(job);
        }
        if state.live_workers < state.maximum_pool_size {
            let worker = self.reserve_worker_locked(&mut state);
            let startup = self.spawn_reserved_worker_with_initial_job_locked(&mut state, worker)?;
            drop(state);
            self.start_reserved_worker(startup, job)
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
        let mut builder =
            thread::Builder::new().name(format!("{}-{index}", self.thread_name_prefix));
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

    /// Spawns a reserved worker that waits for an initial job decision.
    ///
    /// The caller must hold the pool state monitor. The returned startup
    /// channels let the caller run the job's acceptance callback after the
    /// monitor is released, then explicitly release the worker to run or
    /// abort. If the OS thread cannot be created, no job has crossed the
    /// acceptance boundary.
    ///
    /// # Parameters
    ///
    /// * `state` - Locked pool state used for counter updates and rollback.
    /// * `worker` - Worker reservation created while holding the state lock.
    /// # Returns
    ///
    /// Startup channels for the successfully spawned worker.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::WorkerSpawnFailed`] if the worker thread
    /// cannot be created.
    fn spawn_reserved_worker_with_initial_job_locked(
        self: &Arc<Self>,
        state: &mut ThreadPoolState,
        worker: ReservedWorker,
    ) -> Result<InitialWorkerStartup, PoolJobSubmissionError> {
        let ReservedWorker { index } = worker;
        let (start_sender, start_receiver) = mpsc::sync_channel(1);
        let worker_inner = Arc::clone(self);
        let mut builder =
            thread::Builder::new().name(format!("{}-{index}", self.thread_name_prefix));
        if let Some(stack_size) = self.stack_size {
            builder = builder.stack_size(stack_size);
        }
        match builder.spawn(move || match start_receiver.recv() {
            Ok(initial_job) => ThreadPoolWorker::run_initial(worker_inner, index, initial_job),
            Err(_) => {
                let mut state = worker_inner.lock_state();
                worker_inner.unregister_worker_locked(&mut state);
            }
        }) {
            Ok(_) => {
                self.live_worker_count.fetch_add(1, Ordering::Release);
                Ok(InitialWorkerStartup { start_sender })
            }
            Err(source) => {
                state.live_workers = state
                    .live_workers
                    .checked_sub(1)
                    .expect("thread pool live worker counter underflow");
                self.notify_if_idle_or_terminated(state);
                Err(PoolJobSubmissionError::Rejected(
                    SubmissionError::WorkerSpawnFailed {
                        source: Arc::new(source),
                    },
                ))
            }
        }
    }

    /// Runs acceptance for a directly assigned job after the state lock is
    /// released.
    fn start_reserved_worker(
        &self,
        startup: InitialWorkerStartup,
        job: PoolJob,
    ) -> Result<(), PoolJobSubmissionError> {
        let (acceptance_sender, acceptance_receiver) = mpsc::sync_channel(1);
        let (decision_sender, decision_receiver) = mpsc::sync_channel(1);
        startup
            .start_sender
            .send(InitialWorkerJob {
                job,
                acceptance_sender,
                decision_receiver,
            })
            .map_err(|_| PoolJobSubmissionError::Rejected(worker_spawn_failed()))?;

        match acceptance_receiver.recv() {
            Ok(Ok(())) => {
                let decision = {
                    let state = self.lock_state();
                    self.submitted_task_count.fetch_add(1, Ordering::Release);
                    if state.lifecycle == ExecutorServiceLifecycle::Running {
                        self.running_task_count.fetch_add(1, Ordering::Release);
                        InitialWorkerDecision::Run
                    } else {
                        self.cancelling_task_count.fetch_add(1, Ordering::Release);
                        InitialWorkerDecision::Cancel
                    }
                };
                decision_sender
                    .send(decision)
                    .map_err(|_| PoolJobSubmissionError::Rejected(worker_spawn_failed()))?;
                Ok(())
            }
            Ok(Err(())) => {
                let _ignored = decision_sender.send(InitialWorkerDecision::Abort);
                Err(PoolJobSubmissionError::AcceptancePanicked)
            }
            Err(_) => Err(PoolJobSubmissionError::Rejected(worker_spawn_failed())),
        }
    }

    /// Moves a directly assigned task from running to cancellation after a
    /// stop races with the worker's final pre-start check.
    pub(crate) fn begin_cancel_initial_job(&self) {
        let previous = self.running_task_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "thread pool running task counter underflow");
        self.cancelling_task_count.fetch_add(1, Ordering::Release);
    }

    /// Completes cancellation of a directly assigned task.
    pub(crate) fn finish_cancelled_initial_job(&self) {
        let previous = self.cancelling_task_count.fetch_sub(1, Ordering::Release);
        debug_assert!(
            previous > 0,
            "thread pool cancelling task counter underflow"
        );
        self.cancelled_task_count.fetch_add(1, Ordering::Release);
        if self.is_idle_snapshot() {
            self.notify_waiters_after_atomic_change();
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

    /// Returns whether immediate stop has been requested.
    pub(crate) fn is_stopping_now(&self) -> bool {
        self.stop_now.load(Ordering::Acquire)
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
        self.admission.close();
        let mut state = self.lock_state();
        if state.lifecycle == ExecutorServiceLifecycle::Running {
            state.lifecycle = ExecutorServiceLifecycle::ShuttingDown;
        }
        state.notify_all();
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
        self.admission.close();
        self.stop_now.store(true, Ordering::Release);
        let (jobs, queued, running) = {
            let mut state = self.lock_state();
            self.stop_now.store(true, Ordering::Release);
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
                    state.wait();
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
            state.notify_all();
            (jobs, queued, running)
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
            match self.state_monitor.wait_until_ready_with_total_timeout(
                remaining.min(Duration::from_secs(3600)),
                |state| self.is_terminated_locked(state),
            ) {
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
    pub(crate) fn set_keep_alive(
        &self,
        keep_alive: Duration,
    ) -> Result<(), ExecutorServiceBuilderError> {
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
        self.lock_state().notify_all();
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

fn worker_spawn_failed() -> SubmissionError {
    SubmissionError::WorkerSpawnFailed {
        source: Arc::new(std::io::Error::other(
            "worker terminated before startup completed",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::thread;
    use std::time::Duration;

    use qubit_executor::service::ExecutorServiceLifecycle;
    use qubit_executor::service::SubmissionError;

    use super::ThreadPoolConfig;
    use super::ThreadPoolHooks;
    use super::ThreadPoolInner;
    use crate::PoolJob;
    use crate::PoolJobSubmissionError;

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
