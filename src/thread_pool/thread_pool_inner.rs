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
        Mutex,
        atomic::{
            AtomicUsize,
            Ordering,
        },
    },
    thread,
    time::Duration,
};

use crossbeam_deque::{
    Injector,
    Steal,
};

use qubit_lock::{
    Monitor,
    MonitorGuard,
    WaitTimeoutStatus,
};

use super::pool_job::PoolJob;
use super::thread_pool_build_error::ThreadPoolBuildError;
use super::thread_pool_config::ThreadPoolConfig;
use super::thread_pool_lifecycle::ThreadPoolLifecycle;
use super::thread_pool_state::ThreadPoolState;
use super::thread_pool_stats::ThreadPoolStats;
use qubit_executor::service::{
    RejectedExecution,
    ShutdownReport,
};

/// Queue owned by one worker and used for local dispatch plus stealing.
struct WorkerQueue {
    /// Logical worker index used as a stable identity key.
    worker_index: usize,
    /// Lock-free deque of queued jobs assigned to this worker.
    jobs: Injector<PoolJob>,
}

impl WorkerQueue {
    /// Creates an empty local queue for one worker.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker owning this queue.
    ///
    /// # Returns
    ///
    /// A local queue with no jobs.
    fn new(worker_index: usize) -> Self {
        Self {
            worker_index,
            jobs: Injector::new(),
        }
    }

    /// Returns the owning worker index.
    ///
    /// # Returns
    ///
    /// The worker index associated with this queue.
    #[inline]
    fn worker_index(&self) -> usize {
        self.worker_index
    }

    /// Appends a job to the back of this queue.
    ///
    /// # Parameters
    ///
    /// * `job` - Job to enqueue.
    fn push_back(&self, job: PoolJob) {
        self.jobs.push(job);
    }

    /// Pops one job from the front of this queue.
    ///
    /// # Returns
    ///
    /// `Some(job)` when this queue is non-empty, otherwise `None`.
    ///
    /// # Implementation notes
    ///
    /// [`Injector`] does not expose an owner-only pop operation. Both "local
    /// pop" and "remote steal" therefore share the same `steal()` primitive.
    fn pop_front(&self) -> Option<PoolJob> {
        self.steal_one()
    }

    /// Steals one job from the back of this queue.
    ///
    /// # Returns
    ///
    /// `Some(job)` when this queue is non-empty, otherwise `None`.
    ///
    /// # Implementation notes
    ///
    /// This method intentionally reuses [`Self::steal_one`] so all queue
    /// consumers handle transient `Steal::Retry` states consistently.
    fn steal_back(&self) -> Option<PoolJob> {
        self.steal_one()
    }

    /// Drains all queued jobs from this queue.
    ///
    /// # Returns
    ///
    /// A vector containing all queued jobs in FIFO order.
    fn drain(&self) -> Vec<PoolJob> {
        let mut jobs = Vec::new();
        while let Some(job) = self.steal_one() {
            jobs.push(job);
        }
        jobs
    }

    /// Steals one job from this queue and transparently retries transient
    /// contention states.
    ///
    /// # Returns
    ///
    /// `Some(job)` when a job is available, otherwise `None`.
    fn steal_one(&self) -> Option<PoolJob> {
        loop {
            match self.jobs.steal() {
                Steal::Success(job) => return Some(job),
                Steal::Empty => return None,
                // Another thread raced us while mutating queue internals.
                // Retry immediately to mask this transient state from callers.
                Steal::Retry => continue,
            }
        }
    }
}

/// Shared state for a thread pool.
pub(crate) struct ThreadPoolInner {
    /// Mutable pool state protected by a monitor.
    state_monitor: Monitor<ThreadPoolState>,
    /// Registered worker-local queues used for local dispatch and stealing.
    worker_queues: Mutex<Vec<Arc<WorkerQueue>>>,
    /// Round-robin cursor used for queue selection and steal start offsets.
    next_enqueue_worker: AtomicUsize,
    /// Prefix used for naming newly spawned workers.
    thread_name_prefix: String,
    /// Optional stack size in bytes for newly spawned workers.
    stack_size: Option<usize>,
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
    pub(crate) fn new(config: ThreadPoolConfig) -> Self {
        let mut config = config;
        let thread_name_prefix = std::mem::take(&mut config.thread_name_prefix);
        let stack_size = config.stack_size;
        Self {
            state_monitor: Monitor::new(ThreadPoolState::new(config)),
            worker_queues: Mutex::new(Vec::new()),
            next_enqueue_worker: AtomicUsize::new(0),
            thread_name_prefix,
            stack_size,
        }
    }

    /// Acquires the pool state monitor while tolerating poisoned locks.
    ///
    /// # Returns
    ///
    /// A monitor guard for the mutable pool state.
    #[inline]
    pub(crate) fn lock_state(&self) -> MonitorGuard<'_, ThreadPoolState> {
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

    /// Submits a job into the queue.
    ///
    /// # Overall logic
    ///
    /// This method follows a staged admission strategy while holding the pool
    /// monitor lock:
    ///
    /// 1. Reject immediately if the lifecycle is not running.
    /// 2. If live workers are below the core size, spawn a worker and hand the
    ///    job to it directly (no queue hop).
    /// 3. Otherwise, try enqueuing the job if the queue is not saturated.
    /// 4. If the queue is saturated but live workers are still below maximum,
    ///    spawn a non-core worker with the job as its first task.
    /// 5. Otherwise reject as saturated.
    ///
    /// For queued submissions we use a targeted wake-up strategy: wake exactly
    /// one idle worker only when idle workers exist. This avoids the
    /// `notify_all` "thundering herd" effect under high submission rates.
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
    /// Returns [`RejectedExecution::Shutdown`] after shutdown, returns
    /// [`RejectedExecution::Saturated`] when the queue and worker capacity are
    /// full, or returns [`RejectedExecution::WorkerSpawnFailed`] if a required
    /// worker cannot be created.
    pub(crate) fn submit(self: &Arc<Self>, job: PoolJob) -> Result<(), RejectedExecution> {
        let mut state = self.lock_state();
        if !state.lifecycle.is_running() {
            return Err(RejectedExecution::Shutdown);
        }
        if state.live_workers < state.core_pool_size {
            self.spawn_worker_locked(&mut state, Some(job))?;
            state.submitted_tasks += 1;
            return Ok(());
        }
        if !state.is_saturated() {
            state.submitted_tasks += 1;
            state.queued_tasks += 1;
            // Only wake a waiter when at least one worker is currently idle.
            // Busy workers will eventually pull from the queue after finishing
            // their current task, so a broadcast wake-up is unnecessary.
            let should_wake_one_idle_worker = state.idle_workers > 0;
            // Keep unbounded-queue pools on the legacy global-queue fast path.
            // For bounded pools under sustained load and without idle workers,
            // local queues can reduce submit-path contention on the shared
            // global queue.
            let use_local_queue =
                state.queue_capacity.is_some() && state.idle_workers == 0 && state.live_workers > 0;
            if !use_local_queue {
                state.queue.push_back(job);
            } else {
                let mut pending_job = Some(job);
                self.try_enqueue_worker_job_locked(&mut pending_job);
                if let Some(job) = pending_job {
                    state.queue.push_back(job);
                }
            }
            if state.live_workers == 0
                && let Err(error) = self.spawn_worker_locked(&mut state, None)
            {
                if let Some(job) = state.queue.pop_back() {
                    state.submitted_tasks = state
                        .submitted_tasks
                        .checked_sub(1)
                        .expect("thread pool submitted task counter underflow");
                    state.queued_tasks = state
                        .queued_tasks
                        .checked_sub(1)
                        .expect("thread pool queued task counter underflow");
                    drop(state);
                    job.cancel();
                }
                return Err(error);
            }
            // Release the monitor before notifying to keep the critical section
            // short and reduce lock handoff contention.
            drop(state);
            if should_wake_one_idle_worker {
                self.state_monitor.notify_one();
            }
            return Ok(());
        }
        if state.live_workers < state.maximum_pool_size {
            self.spawn_worker_locked(&mut state, Some(job))?;
            state.submitted_tasks += 1;
            Ok(())
        } else {
            Err(RejectedExecution::Saturated)
        }
    }

    /// Tries to enqueue a job into one worker-local queue.
    ///
    /// # Parameters
    ///
    /// * `job` - Slot containing one pending job. This method moves the job
    ///   out of the slot on success.
    ///
    /// # Returns
    ///
    /// `true` when a worker-local queue accepted the job, otherwise `false`.
    ///
    /// # Overall logic
    ///
    /// The submit path keeps this operation O(1): choose exactly one local
    /// queue with a round-robin cursor and push once, then fallback to the
    /// global queue when local enqueue is unavailable.
    fn try_enqueue_worker_job_locked(&self, job: &mut Option<PoolJob>) -> bool {
        let queues = self
            .worker_queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if queues.is_empty() {
            return false;
        }
        let start = self.next_enqueue_worker.fetch_add(1, Ordering::Relaxed);
        let slot = start % queues.len();
        if let Some(job) = job.take() {
            queues[slot].push_back(job);
            true
        } else {
            false
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
    /// Returns [`RejectedExecution::Shutdown`] after shutdown or
    /// [`RejectedExecution::WorkerSpawnFailed`] if the worker cannot be
    /// created.
    pub(crate) fn prestart_core_thread(self: &Arc<Self>) -> Result<bool, RejectedExecution> {
        let mut state = self.lock_state();
        if !state.lifecycle.is_running() {
            return Err(RejectedExecution::Shutdown);
        }
        if state.live_workers >= state.core_pool_size {
            return Ok(false);
        }
        self.spawn_worker_locked(&mut state, None)?;
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
    /// Returns [`RejectedExecution`] if shutdown is observed or a worker cannot
    /// be created.
    pub(crate) fn prestart_all_core_threads(self: &Arc<Self>) -> Result<usize, RejectedExecution> {
        let mut started = 0;
        while self.prestart_core_thread()? {
            started += 1;
        }
        Ok(started)
    }

    /// Spawns a worker while the caller holds the pool state lock.
    ///
    /// # Parameters
    ///
    /// * `state` - Locked mutable pool state to update while spawning.
    /// * `first_task` - Optional first job assigned directly to the new worker.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the worker thread is spawned.
    ///
    /// # Errors
    ///
    /// Returns [`RejectedExecution::WorkerSpawnFailed`] if
    /// [`thread::Builder::spawn`] fails.
    fn spawn_worker_locked(
        self: &Arc<Self>,
        state: &mut ThreadPoolState,
        first_task: Option<PoolJob>,
    ) -> Result<(), RejectedExecution> {
        let index = state.next_worker_index;
        state.next_worker_index += 1;
        state.live_workers += 1;
        if first_task.is_some() {
            state.running_tasks += 1;
        }
        let worker_queue = self.register_worker_queue_locked(index);

        let worker_inner = Arc::clone(self);
        let mut builder =
            thread::Builder::new().name(format!("{}-{index}", self.thread_name_prefix));
        if let Some(stack_size) = self.stack_size {
            builder = builder.stack_size(stack_size);
        }
        match builder.spawn(move || run_worker(worker_inner, worker_queue, first_task)) {
            Ok(_) => Ok(()),
            Err(source) => {
                // Roll back the queue registration because this worker never
                // reached the run loop.
                self.remove_worker_queue_locked(index);
                state.live_workers = state
                    .live_workers
                    .checked_sub(1)
                    .expect("thread pool live worker counter underflow");
                if state.running_tasks > 0 {
                    state.running_tasks -= 1;
                }
                self.notify_if_terminated(state);
                Err(RejectedExecution::WorkerSpawnFailed {
                    source: Arc::new(source),
                })
            }
        }
    }

    /// Registers an empty worker-local queue for a newly spawned worker.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the new worker.
    fn register_worker_queue_locked(&self, worker_index: usize) -> Arc<WorkerQueue> {
        let queue = Arc::new(WorkerQueue::new(worker_index));
        self.worker_queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::clone(&queue));
        queue
    }

    /// Removes one worker-local queue and returns all jobs still queued in it.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the retiring worker.
    ///
    /// # Returns
    ///
    /// Remaining queued jobs from the removed queue, if any.
    fn remove_worker_queue_locked(&self, worker_index: usize) -> Vec<PoolJob> {
        let queue = {
            let mut queues = self
                .worker_queues
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            queues
                .iter()
                .position(|queue| queue.worker_index() == worker_index)
                .map(|position| queues.remove(position))
        };
        queue.map_or_else(Vec::new, |queue| queue.drain())
    }

    /// Attempts to take one queued job for the specified worker.
    ///
    /// # Overall logic
    ///
    /// The lookup order favors locality first and balance second:
    ///
    /// 1. Pop from the worker's own local queue.
    /// 2. Steal from other workers' local queues (bounded pools only).
    /// 3. Pop from the global fallback queue.
    ///
    /// This method mutates queue-related counters only after a job is
    /// successfully claimed.
    ///
    /// # Parameters
    ///
    /// * `state` - Locked mutable pool state.
    /// * `worker_queue` - Local queue owned by the worker requesting work.
    ///
    /// # Returns
    ///
    /// `Some(job)` when any queue has work, otherwise `None`.
    fn try_take_queued_job_locked(
        &self,
        state: &mut ThreadPoolState,
        worker_queue: &WorkerQueue,
    ) -> Option<PoolJob> {
        if state.queue_capacity.is_some() {
            let worker_index = worker_queue.worker_index();
            let own_job = worker_queue.pop_front();
            if let Some(job) = own_job {
                state.queued_tasks = state
                    .queued_tasks
                    .checked_sub(1)
                    .expect("thread pool queued task counter underflow");
                state.running_tasks += 1;
                return Some(job);
            }

            if let Some(job) = self.try_steal_job_locked(worker_index) {
                state.queued_tasks = state
                    .queued_tasks
                    .checked_sub(1)
                    .expect("thread pool queued task counter underflow");
                state.running_tasks += 1;
                return Some(job);
            }
        }

        if let Some(job) = state.queue.pop_front() {
            state.queued_tasks = state
                .queued_tasks
                .checked_sub(1)
                .expect("thread pool queued task counter underflow");
            state.running_tasks += 1;
            return Some(job);
        }

        None
    }

    /// Attempts to steal one queued job from another worker queue.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Worker that is requesting stolen work.
    ///
    /// # Returns
    ///
    /// `Some(job)` when any other worker queue can provide one job.
    fn try_steal_job_locked(&self, worker_index: usize) -> Option<PoolJob> {
        let queues = self
            .worker_queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let queue_count = queues.len();
        if queue_count <= 1 {
            return None;
        }
        // Rotate victim probing start index to avoid repeatedly hammering the
        // same queue under contention.
        let start = self.next_enqueue_worker.fetch_add(1, Ordering::Relaxed) % queue_count;
        for offset in 0..queue_count {
            let victim = &queues[(start + offset) % queue_count];
            if victim.worker_index() == worker_index {
                continue;
            }
            if let Some(job) = victim.steal_back() {
                return Some(job);
            }
        }
        None
    }

    /// Drains all jobs from all worker-local queues.
    ///
    /// # Returns
    ///
    /// A vector containing every job drained from worker-local queues.
    fn drain_all_worker_queued_jobs_locked(&self) -> Vec<PoolJob> {
        let queues = self
            .worker_queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut jobs = Vec::new();
        for queue in queues {
            jobs.extend(queue.drain());
        }
        jobs
    }

    /// Requests graceful shutdown.
    ///
    /// The pool rejects later submissions but lets queued work drain.
    pub(crate) fn shutdown(&self) {
        let mut state = self.lock_state();
        if state.lifecycle.is_running() {
            state.lifecycle = ThreadPoolLifecycle::Shutdown;
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
    pub(crate) fn shutdown_now(&self) -> ShutdownReport {
        let (jobs, report) = {
            let mut state = self.lock_state();
            if state.lifecycle.is_running() || state.lifecycle.is_shutdown() {
                state.lifecycle = ThreadPoolLifecycle::Stopping;
            }
            let queued = state.queued_tasks;
            let running = state.running_tasks;
            let mut jobs = state.queue.drain(..).collect::<Vec<_>>();
            jobs.extend(self.drain_all_worker_queued_jobs_locked());
            debug_assert_eq!(jobs.len(), queued);
            state.queued_tasks = 0;
            state.cancelled_tasks += queued;
            self.state_monitor.notify_all();
            self.notify_if_terminated(&state);
            (jobs, ShutdownReport::new(queued, running, queued))
        };
        for job in jobs {
            job.cancel();
        }
        report
    }

    /// Returns whether shutdown has been requested.
    ///
    /// # Returns
    ///
    /// `true` if the pool is no longer in the running lifecycle state.
    pub(crate) fn is_shutdown(&self) -> bool {
        self.read_state(|state| !state.lifecycle.is_running())
    }

    /// Returns whether the pool is fully terminated.
    ///
    /// # Returns
    ///
    /// `true` if shutdown has started and no queued, running, or live worker
    /// state remains.
    pub(crate) fn is_terminated(&self) -> bool {
        self.read_state(ThreadPoolState::is_terminated)
    }

    /// Blocks the current thread until this pool is terminated.
    ///
    /// This method waits on a condition variable and therefore blocks the
    /// calling thread.
    pub(crate) fn wait_for_termination(&self) {
        self.state_monitor
            .wait_until(|state| state.is_terminated(), |_| ());
    }

    /// Returns a point-in-time pool snapshot.
    ///
    /// # Returns
    ///
    /// A snapshot built while holding the pool state lock.
    pub(crate) fn stats(&self) -> ThreadPoolStats {
        self.read_state(ThreadPoolStats::new)
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
    /// Returns [`ThreadPoolBuildError::CorePoolSizeExceedsMaximum`] when the
    /// new core size is greater than the current maximum size.
    pub(crate) fn set_core_pool_size(
        self: &Arc<Self>,
        core_pool_size: usize,
    ) -> Result<(), ThreadPoolBuildError> {
        let err = self.write_state(|state| {
            if core_pool_size > state.maximum_pool_size {
                Some(state.maximum_pool_size)
            } else {
                state.core_pool_size = core_pool_size;
                None
            }
        });
        if let Some(maximum_pool_size) = err {
            return Err(ThreadPoolBuildError::CorePoolSizeExceedsMaximum {
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
    /// Returns [`ThreadPoolBuildError::ZeroMaximumPoolSize`] for zero, or
    /// [`ThreadPoolBuildError::CorePoolSizeExceedsMaximum`] when the current
    /// core size is greater than the new maximum size.
    pub(crate) fn set_maximum_pool_size(
        self: &Arc<Self>,
        maximum_pool_size: usize,
    ) -> Result<(), ThreadPoolBuildError> {
        if maximum_pool_size == 0 {
            return Err(ThreadPoolBuildError::ZeroMaximumPoolSize);
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
            return Err(ThreadPoolBuildError::CorePoolSizeExceedsMaximum {
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
    /// Returns [`ThreadPoolBuildError::ZeroKeepAlive`] when the duration is
    /// zero.
    pub(crate) fn set_keep_alive(&self, keep_alive: Duration) -> Result<(), ThreadPoolBuildError> {
        if keep_alive.is_zero() {
            return Err(ThreadPoolBuildError::ZeroKeepAlive);
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

    /// Notifies termination waiters when the state is terminal.
    ///
    /// # Parameters
    ///
    /// * `state` - Current pool state observed while holding the state lock.
    fn notify_if_terminated(&self, state: &ThreadPoolState) {
        if state.is_terminated() {
            self.state_monitor.notify_all();
        }
    }
}

/// Runs a single worker loop until the pool asks it to exit.
///
/// # Parameters
///
/// * `inner` - Shared pool state used for queue access and counters.
/// * `worker_queue` - Local queue owned by this worker.
/// * `first_task` - Optional job assigned directly when the worker is spawned.
fn run_worker(
    inner: Arc<ThreadPoolInner>,
    worker_queue: Arc<WorkerQueue>,
    first_task: Option<PoolJob>,
) {
    if let Some(job) = first_task {
        job.run();
        finish_running_job(&inner);
    }
    loop {
        let job = wait_for_job(&inner, &worker_queue);
        match job {
            Some(job) => {
                job.run();
                finish_running_job(&inner);
            }
            None => return,
        }
    }
}

/// Waits until a worker can take a job or should exit.
///
/// # Parameters
///
/// * `inner` - Shared pool state and monitor wait queue.
/// * `worker_queue` - Local queue owned by the worker requesting a job.
///
/// # Returns
///
/// `Some(job)` when work is available, or `None` when the worker should exit.
fn wait_for_job(inner: &ThreadPoolInner, worker_queue: &WorkerQueue) -> Option<PoolJob> {
    let worker_index = worker_queue.worker_index();
    let mut state = inner.lock_state();
    loop {
        match state.lifecycle {
            ThreadPoolLifecycle::Running => {
                if let Some(job) = inner.try_take_queued_job_locked(&mut state, worker_queue) {
                    return Some(job);
                }
                if state.live_workers > state.maximum_pool_size && state.live_workers > 0 {
                    unregister_exiting_worker(inner, &mut state, worker_index);
                    return None;
                }
                if state.worker_wait_is_timed() {
                    let keep_alive = state.keep_alive;
                    state.idle_workers += 1;
                    let (next_state, status) = state.wait_timeout(keep_alive);
                    state = next_state;
                    state.idle_workers = state
                        .idle_workers
                        .checked_sub(1)
                        .expect("thread pool idle worker counter underflow");
                    if status == WaitTimeoutStatus::TimedOut
                        && state.queued_tasks == 0
                        && state.idle_worker_can_retire()
                    {
                        unregister_exiting_worker(inner, &mut state, worker_index);
                        return None;
                    }
                } else {
                    state.idle_workers += 1;
                    state = state.wait();
                    state.idle_workers = state
                        .idle_workers
                        .checked_sub(1)
                        .expect("thread pool idle worker counter underflow");
                }
            }
            ThreadPoolLifecycle::Shutdown => {
                if let Some(job) = inner.try_take_queued_job_locked(&mut state, worker_queue) {
                    return Some(job);
                }
                unregister_exiting_worker(inner, &mut state, worker_index);
                return None;
            }
            ThreadPoolLifecycle::Stopping => {
                unregister_exiting_worker(inner, &mut state, worker_index);
                return None;
            }
        }
    }
}

/// Marks a worker-held job as finished.
///
/// # Parameters
///
/// * `inner` - Shared pool state whose running and completed counters are
///   updated.
fn finish_running_job(inner: &ThreadPoolInner) {
    let mut state = inner.lock_state();
    state.running_tasks = state
        .running_tasks
        .checked_sub(1)
        .expect("thread pool running task counter underflow");
    state.completed_tasks += 1;
    inner.notify_if_terminated(&state);
}

/// Marks a worker as exited.
///
/// # Parameters
///
/// * `inner` - Shared pool coordination state used for termination
///   notification.
/// * `state` - Locked mutable state whose live worker count is decremented.
/// * `worker_index` - Stable index of the exiting worker.
fn unregister_exiting_worker(
    inner: &ThreadPoolInner,
    state: &mut ThreadPoolState,
    worker_index: usize,
) {
    // Migrate leftover local jobs back to the global queue before removing the
    // worker registration so queued work is not lost while this worker retires.
    let requeued_jobs = inner.remove_worker_queue_locked(worker_index);
    for job in requeued_jobs {
        state.queue.push_back(job);
    }
    state.live_workers = state
        .live_workers
        .checked_sub(1)
        .expect("thread pool live worker counter underflow");
    inner.notify_if_terminated(state);
}
