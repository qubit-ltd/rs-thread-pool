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

use qubit_executor::service::{
    ExecutorServiceLifecycle,
    StopReport,
    SubmissionError,
};
use qubit_lock::{
    Monitor,
    MonitorGuard,
};

use super::thread_pool_config::ThreadPoolConfig;
use super::thread_pool_state::ThreadPoolState;
use super::thread_pool_worker::ThreadPoolWorker;
use super::thread_pool_worker_queue::ThreadPoolWorkerQueue;
use super::thread_pool_worker_runtime::ThreadPoolWorkerRuntime;
use crate::{
    ExecutorServiceBuilderError,
    PoolJob,
    ThreadPoolHooks,
    ThreadPoolStats,
};

/// Shared state for a thread pool.
pub(crate) struct ThreadPoolInner {
    /// Mutable pool state protected by a monitor.
    state_monitor: Monitor<ThreadPoolState>,
    /// Registered worker-local queues used for local dispatch and stealing.
    worker_queues: Mutex<Vec<Arc<ThreadPoolWorkerQueue>>>,
    /// Round-robin cursor used for queue selection and steal start offsets.
    next_enqueue_worker: AtomicUsize,
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
        Self {
            state_monitor: Monitor::new(ThreadPoolState::new(config)),
            worker_queues: Mutex::new(Vec::new()),
            next_enqueue_worker: AtomicUsize::new(0),
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
    /// Returns [`SubmissionError::Shutdown`] after shutdown, returns
    /// [`SubmissionError::Saturated`] when the queue and worker capacity are
    /// full, or returns [`SubmissionError::WorkerSpawnFailed`] if a required
    /// worker cannot be created.
    pub(crate) fn submit(self: &Arc<Self>, job: PoolJob) -> Result<(), SubmissionError> {
        let mut state = self.lock_state();
        if state.lifecycle != ExecutorServiceLifecycle::Running {
            return Err(SubmissionError::Shutdown);
        }
        if state.live_workers < state.core_pool_size {
            job.accept();
            state.submitted_tasks += 1;
            let worker = self.reserve_worker_locked(&mut state, Some(job));
            drop(state);
            if let Err(error) = self.spawn_reserved_worker(worker) {
                self.rollback_submitted_task();
                return Err(error);
            }
            return Ok(());
        }
        if !state.is_saturated() {
            job.accept();
            state.submitted_tasks += 1;
            state.queued_tasks += 1;
            // Only wake a waiter when at least one worker is currently idle.
            // Busy workers will eventually pull from the queue after finishing
            // their current task, so a broadcast wake-up is unnecessary.
            let should_wake_one_idle_worker = state.idle_workers > 0;
            state.queue.push_back(job);
            if state.live_workers == 0 {
                let worker = self.reserve_worker_locked(&mut state, None);
                drop(state);
                if let Err(error) = self.spawn_reserved_worker(worker) {
                    let mut state = self.lock_state();
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
                return Ok(());
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
            job.accept();
            state.submitted_tasks += 1;
            let worker = self.reserve_worker_locked(&mut state, Some(job));
            drop(state);
            if let Err(error) = self.spawn_reserved_worker(worker) {
                self.rollback_submitted_task();
                return Err(error);
            }
            Ok(())
        } else {
            Err(SubmissionError::Saturated)
        }
    }

    /// Rolls back one submitted-task count after worker spawn failure.
    fn rollback_submitted_task(&self) {
        self.write_state(|state| {
            state.submitted_tasks = state
                .submitted_tasks
                .checked_sub(1)
                .expect("thread pool submitted task counter underflow");
        });
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
        let worker = self.reserve_worker_locked(&mut state, None);
        drop(state);
        self.spawn_reserved_worker(worker)?;
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
    /// * `first_task` - Optional first job assigned directly to the new worker.
    ///
    /// # Returns
    ///
    /// A worker reservation ready to spawn after releasing the lock.
    fn reserve_worker_locked(
        self: &Arc<Self>,
        state: &mut ThreadPoolState,
        first_task: Option<PoolJob>,
    ) -> ReservedWorker {
        let index = state.next_worker_index;
        state.next_worker_index += 1;
        state.live_workers += 1;
        let runtime = self.register_worker_queue_locked(index);
        let has_initial_job = first_task.is_some();
        if let Some(job) = first_task {
            runtime.push_back(job);
            state.queued_tasks += 1;
        }
        ReservedWorker {
            index,
            runtime,
            has_initial_job,
        }
    }

    /// Spawns a previously reserved worker without holding the state lock.
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
    fn spawn_reserved_worker(
        self: &Arc<Self>,
        worker: ReservedWorker,
    ) -> Result<(), SubmissionError> {
        let ReservedWorker {
            index,
            runtime,
            has_initial_job,
        } = worker;
        let worker_inner = Arc::clone(self);
        let mut builder =
            thread::Builder::new().name(format!("{}-{index}", self.thread_name_prefix));
        if let Some(stack_size) = self.stack_size {
            builder = builder.stack_size(stack_size);
        }
        match builder.spawn(move || {
            ThreadPoolWorker::run(worker_inner, runtime);
        }) {
            Ok(_) => Ok(()),
            Err(source) => {
                let mut state = self.lock_state();
                let cancelled_jobs = self.remove_worker_queue_locked(index);
                state.live_workers = state
                    .live_workers
                    .checked_sub(1)
                    .expect("thread pool live worker counter underflow");
                if has_initial_job {
                    state.queued_tasks = state
                        .queued_tasks
                        .checked_sub(cancelled_jobs.len())
                        .expect("thread pool queued task counter underflow");
                }
                self.notify_if_terminated(&state);
                drop(state);
                for job in cancelled_jobs {
                    job.cancel();
                }
                Err(SubmissionError::WorkerSpawnFailed {
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
    fn register_worker_queue_locked(&self, worker_index: usize) -> ThreadPoolWorkerRuntime {
        let runtime = ThreadPoolWorkerRuntime::new(worker_index);
        self.worker_queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(runtime.queue());
        runtime
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
    pub(crate) fn remove_worker_queue_locked(&self, worker_index: usize) -> Vec<PoolJob> {
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
    pub(crate) fn try_take_queued_job_locked(
        &self,
        state: &mut ThreadPoolState,
        worker_runtime: &ThreadPoolWorkerRuntime,
    ) -> Option<PoolJob> {
        let worker_index = worker_runtime.worker_index();
        let own_job = worker_runtime.pop_front();
        if let Some(job) = own_job {
            state.queued_tasks = state
                .queued_tasks
                .checked_sub(1)
                .expect("thread pool queued task counter underflow");
            state.running_tasks += 1;
            return Some(job);
        }

        if state.queue_capacity.is_some()
            && let Some(job) = self.try_steal_job_locked(worker_index)
        {
            state.queued_tasks = state
                .queued_tasks
                .checked_sub(1)
                .expect("thread pool queued task counter underflow");
            state.running_tasks += 1;
            return Some(job);
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
        let (jobs, report) = {
            let mut state = self.lock_state();
            if matches!(
                state.lifecycle,
                ExecutorServiceLifecycle::Running | ExecutorServiceLifecycle::ShuttingDown
            ) {
                state.lifecycle = ExecutorServiceLifecycle::Stopping;
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
            (jobs, StopReport::new(queued, running, queued))
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
            if state.is_terminated() {
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

    /// Blocks until all currently accepted work has completed.
    ///
    /// This method waits for queued and running tasks to drain, but it does not
    /// request shutdown and does not wait for worker threads to exit.
    pub(crate) fn wait_until_idle(&self) {
        self.state_monitor
            .wait_until(|state| state.is_idle(), |_| ());
    }

    /// Returns a point-in-time pool snapshot.
    ///
    /// # Returns
    ///
    /// A snapshot built while holding the pool state lock.
    pub(crate) fn stats(&self) -> ThreadPoolStats {
        self.read_state(ThreadPoolState::stats)
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

    /// Notifies termination waiters when the state is terminal.
    ///
    /// # Parameters
    ///
    /// * `state` - Current pool state observed while holding the state lock.
    pub(crate) fn notify_if_terminated(&self, state: &ThreadPoolState) {
        if state.is_terminated() {
            self.state_monitor.notify_all();
        }
    }

    /// Notifies waiters when the pool is idle or fully terminated.
    ///
    /// # Parameters
    ///
    /// * `state` - Current pool state observed while holding the state lock.
    pub(crate) fn notify_if_idle_or_terminated(&self, state: &ThreadPoolState) {
        if state.is_idle() || state.is_terminated() {
            self.state_monitor.notify_all();
        }
    }
}

/// Worker reservation created under the pool state lock and spawned after the
/// lock is released.
struct ReservedWorker {
    /// Stable worker index assigned in pool state.
    index: usize,
    /// Worker-local runtime owning the local queue.
    runtime: ThreadPoolWorkerRuntime,
    /// Whether one initial job was placed into the worker-local deque.
    has_initial_job: bool,
}
