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
// qubit-style: allow explicit-imports
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use crossbeam_deque::Injector;
use qubit_function::Callable;

use qubit_executor::{TaskCompletionPair, TaskHandle};
use qubit_lock::Monitor;

use super::fixed_thread_pool_builder::FixedThreadPoolBuilder;
use super::queue_steal_source::{steal_batch_and_pop, steal_one};
use super::thread_pool::{ThreadPoolBuildError, ThreadPoolStats};
use super::worker_queue::WorkerQueue;
use super::worker_runtime::WorkerRuntime;
use crate::thread_pool::PoolJob;
use qubit_executor::service::{ExecutorService, RejectedExecution, ShutdownReport};

/// Maximum number of worker-local queues probed by one submit call.
const LOCAL_ENQUEUE_MAX_PROBES: usize = 4;
/// Maximum worker count that uses worker-local batch queues.
const LOCAL_QUEUE_WORKER_LIMIT: usize = 4;

/// Lifecycle state for a fixed-size thread pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixedThreadPoolLifecycle {
    /// The pool accepts new tasks and workers wait for queued work.
    Running,

    /// The pool rejects new tasks but drains queued work.
    Shutdown,

    /// The pool rejects new tasks and cancels queued work.
    Stopping,
}

impl FixedThreadPoolLifecycle {
    /// Returns whether this lifecycle still accepts submissions.
    ///
    /// # Returns
    ///
    /// `true` only while the pool is running.
    const fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

/// Mutable state protected by the fixed pool monitor.
struct FixedThreadPoolState {
    /// Current lifecycle state.
    lifecycle: FixedThreadPoolLifecycle,
    /// Number of worker loops that have not exited.
    live_workers: usize,
    /// Number of workers currently blocked waiting for work.
    idle_workers: usize,
}

impl FixedThreadPoolState {
    /// Creates an empty running state.
    ///
    /// # Returns
    ///
    /// A running state before any worker has been reserved.
    fn new() -> Self {
        Self {
            lifecycle: FixedThreadPoolLifecycle::Running,
            live_workers: 0,
            idle_workers: 0,
        }
    }
}

/// Shared state for a fixed-size thread pool.
struct FixedThreadPoolInner {
    /// Number of workers in this fixed pool.
    pool_size: usize,
    /// Mutable lifecycle and worker counters.
    state: Monitor<FixedThreadPoolState>,
    /// Admission gate used by submitters.
    accepting: AtomicBool,
    /// Whether immediate shutdown has requested workers to stop taking jobs.
    stop_now: AtomicBool,
    /// Submit calls that have passed the first admission check.
    inflight_submissions: AtomicUsize,
    /// Number of workers currently blocked or about to block waiting for work.
    idle_worker_count: AtomicUsize,
    /// Number of idle-worker wakeups already requested but not yet consumed.
    pending_worker_wakes: AtomicUsize,
    /// Lock-free queue for externally submitted jobs.
    global_queue: Injector<PoolJob>,
    /// Worker-local queues used for submit routing and work stealing.
    worker_queues: Vec<Arc<WorkerQueue>>,
    /// Round-robin cursor used for submit-path local queue selection.
    next_enqueue_worker: AtomicUsize,
    /// Optional maximum number of queued jobs.
    queue_capacity: Option<usize>,
    /// Number of queued jobs not yet started or cancelled.
    queued_task_count: AtomicUsize,
    /// Number of jobs currently running.
    running_task_count: AtomicUsize,
    /// Total number of accepted jobs.
    submitted_task_count: AtomicUsize,
    /// Total number of finished worker-held jobs.
    completed_task_count: AtomicUsize,
    /// Total number of queued jobs cancelled by immediate shutdown.
    cancelled_task_count: AtomicUsize,
}

impl FixedThreadPoolInner {
    /// Creates shared state for a fixed-size pool.
    ///
    /// # Parameters
    ///
    /// * `pool_size` - Number of workers that will be prestarted.
    /// * `queue_capacity` - Optional queue capacity.
    ///
    /// # Returns
    ///
    /// A shared state object ready for worker startup.
    fn new(
        pool_size: usize,
        queue_capacity: Option<usize>,
        worker_queues: Vec<Arc<WorkerQueue>>,
    ) -> Self {
        Self {
            pool_size,
            state: Monitor::new(FixedThreadPoolState::new()),
            accepting: AtomicBool::new(true),
            stop_now: AtomicBool::new(false),
            inflight_submissions: AtomicUsize::new(0),
            idle_worker_count: AtomicUsize::new(0),
            pending_worker_wakes: AtomicUsize::new(0),
            global_queue: Injector::new(),
            worker_queues,
            next_enqueue_worker: AtomicUsize::new(0),
            queue_capacity,
            queued_task_count: AtomicUsize::new(0),
            running_task_count: AtomicUsize::new(0),
            submitted_task_count: AtomicUsize::new(0),
            completed_task_count: AtomicUsize::new(0),
            cancelled_task_count: AtomicUsize::new(0),
        }
    }

    /// Returns the fixed worker count.
    ///
    /// # Returns
    ///
    /// Number of workers owned by this pool.
    #[inline]
    fn pool_size(&self) -> usize {
        self.pool_size
    }

    /// Returns the queued task count.
    ///
    /// # Returns
    ///
    /// Number of accepted tasks waiting to run.
    #[inline]
    fn queued_count(&self) -> usize {
        self.queued_task_count.load(Ordering::Acquire)
    }

    /// Returns the running task count.
    ///
    /// # Returns
    ///
    /// Number of tasks currently held by workers.
    #[inline]
    fn running_count(&self) -> usize {
        self.running_task_count.load(Ordering::Acquire)
    }

    /// Returns the number of in-flight submit calls.
    ///
    /// # Returns
    ///
    /// Number of submit calls that may still publish or roll back a queued job.
    #[inline]
    fn inflight_count(&self) -> usize {
        self.inflight_submissions.load(Ordering::Acquire)
    }

    /// Attempts to enter submit admission.
    ///
    /// # Returns
    ///
    /// A guard that leaves admission on drop.
    ///
    /// # Errors
    ///
    /// Returns [`RejectedExecution::Shutdown`] when admission is closed.
    fn begin_submit(&self) -> Result<FixedSubmitGuard<'_>, RejectedExecution> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(RejectedExecution::Shutdown);
        }
        self.inflight_submissions.fetch_add(1, Ordering::AcqRel);
        if self.accepting.load(Ordering::Acquire) {
            Ok(FixedSubmitGuard { inner: self })
        } else {
            let previous = self.inflight_submissions.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "fixed pool submit counter underflow");
            if previous == 1 {
                self.state.notify_all();
            }
            Err(RejectedExecution::Shutdown)
        }
    }

    /// Attempts to reserve one queue slot.
    ///
    /// # Returns
    ///
    /// `true` if one queued slot was reserved, otherwise `false`.
    fn reserve_queue_slot(&self) -> bool {
        if let Some(capacity) = self.queue_capacity {
            loop {
                let current = self.queued_count();
                if current >= capacity {
                    return false;
                }
                if self
                    .queued_task_count
                    .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return true;
                }
            }
        }
        self.queued_task_count.fetch_add(1, Ordering::AcqRel);
        true
    }

    /// Submits one job to this fixed pool.
    ///
    /// # Parameters
    ///
    /// * `job` - Type-erased job accepted by the pool.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the job is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`RejectedExecution::Shutdown`] after shutdown or
    /// [`RejectedExecution::Saturated`] when the bounded queue is full.
    fn submit(&self, job: PoolJob) -> Result<(), RejectedExecution> {
        let _guard = self.begin_submit()?;
        if !self.reserve_queue_slot() {
            return Err(RejectedExecution::Saturated);
        }
        if !self.accepting.load(Ordering::Acquire) {
            let previous = self.queued_task_count.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "fixed pool queued counter underflow");
            return Err(RejectedExecution::Shutdown);
        }
        self.submitted_task_count.fetch_add(1, Ordering::Relaxed);
        self.enqueue_job(job);
        Ok(())
    }

    /// Enqueues one accepted job to a worker inbox or the global fallback.
    ///
    /// # Parameters
    ///
    /// * `job` - Job whose queued slot has already been reserved.
    fn enqueue_job(&self, job: PoolJob) {
        if self.use_worker_local_queues() {
            match self.try_enqueue_to_worker(job) {
                Ok(()) => {}
                Err(job) => self.global_queue.push(job),
            }
        } else {
            self.global_queue.push(job);
        }
        self.wake_one_idle_worker();
    }

    /// Wakes one idle worker if no already-requested wakeup covers it.
    ///
    /// Pending wake tokens close the lost-notification window: a worker that
    /// has marked itself idle but has not yet parked will observe the token and
    /// retry work without relying on the condition-variable notification.
    fn wake_one_idle_worker(&self) {
        loop {
            let idle_workers = self.idle_worker_count.load(Ordering::Acquire);
            if idle_workers == 0 {
                return;
            }
            let pending_wakes = self.pending_worker_wakes.load(Ordering::Acquire);
            if pending_wakes >= idle_workers {
                return;
            }
            if self
                .pending_worker_wakes
                .compare_exchange_weak(
                    pending_wakes,
                    pending_wakes + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                self.state.notify_one();
                return;
            }
        }
    }

    /// Returns whether an idle-worker wakeup has been requested.
    ///
    /// # Returns
    ///
    /// `true` when at least one idle worker should leave the wait path and
    /// retry taking work.
    fn has_pending_worker_wake(&self) -> bool {
        self.pending_worker_wakes.load(Ordering::Acquire) > 0
    }

    /// Consumes one requested idle-worker wakeup if one exists.
    fn consume_pending_worker_wake(&self) {
        let mut current = self.pending_worker_wakes.load(Ordering::Acquire);
        while current > 0 {
            match self.pending_worker_wakes.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// Attempts to route one job directly to an active worker queue.
    ///
    /// # Parameters
    ///
    /// * `job` - Job to route.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the job was published to a worker inbox; otherwise the
    /// original job is returned for global fallback.
    fn try_enqueue_to_worker(&self, job: PoolJob) -> Result<(), PoolJob> {
        let queue_count = self.worker_queues.len();
        debug_assert!(queue_count > 0, "fixed pool must have worker queues");
        let probe_count = queue_count.min(LOCAL_ENQUEUE_MAX_PROBES);
        for _ in 0..probe_count {
            let index = self.next_enqueue_worker.fetch_add(1, Ordering::Relaxed) % queue_count;
            let queue = &self.worker_queues[index];
            if queue.is_active() {
                queue.push_back(job);
                return Ok(());
            }
        }
        Err(job)
    }

    /// Attempts to claim one queued job for a worker.
    ///
    /// The worker first checks its local queue, then its cross-thread inbox,
    /// then the global fallback queue, and finally steals from other workers.
    /// This matches the dynamic pool's hot path and avoids forcing all fixed
    /// workers through one global injector under skewed workloads.
    ///
    /// # Parameters
    ///
    /// * `worker_runtime` - Queue runtime owned by the current worker.
    ///
    /// # Returns
    ///
    /// `Some(job)` when a job was claimed, otherwise `None`.
    fn try_take_job(&self, worker_runtime: &WorkerRuntime) -> Option<PoolJob> {
        if self.stop_now.load(Ordering::Acquire) {
            self.cancel_worker_jobs(worker_runtime);
            return None;
        }
        if !self.use_worker_local_queues() {
            return self.steal_single_global_job(worker_runtime);
        }
        if let Some(job) = worker_runtime.local.pop() {
            return self.accept_claimed_job(job, worker_runtime);
        }
        if let Some(job) = worker_runtime.queue.pop_inbox_into(&worker_runtime.local) {
            return self.accept_claimed_job(job, worker_runtime);
        }
        if let Some(job) = self.steal_global_job(worker_runtime) {
            return Some(job);
        }
        self.steal_worker_job(worker_runtime)
    }

    /// Attempts to batch-steal one job from the global injector.
    ///
    /// # Parameters
    ///
    /// * `worker_runtime` - Queue runtime receiving any stolen batch remainder.
    ///
    /// # Returns
    ///
    /// `Some(job)` when a job was claimed, otherwise `None`.
    fn steal_global_job(&self, worker_runtime: &WorkerRuntime) -> Option<PoolJob> {
        if let Some(job) = steal_batch_and_pop(&self.global_queue, &worker_runtime.local) {
            if !worker_runtime.local.is_empty() {
                self.state.notify_one();
            }
            return self.accept_claimed_job(job, worker_runtime);
        }
        self.steal_single_global_job(worker_runtime)
    }

    /// Attempts to steal exactly one job from the global injector.
    ///
    /// # Parameters
    ///
    /// * `worker_runtime` - Queue runtime owned by the current worker.
    ///
    /// # Returns
    ///
    /// `Some(job)` when a job was claimed, otherwise `None`.
    fn steal_single_global_job(&self, worker_runtime: &WorkerRuntime) -> Option<PoolJob> {
        steal_one(&self.global_queue).and_then(|job| self.accept_claimed_job(job, worker_runtime))
    }

    /// Attempts to steal one job from another worker's local queue.
    ///
    /// # Parameters
    ///
    /// * `worker_runtime` - Queue runtime owned by the current worker.
    ///
    /// # Returns
    ///
    /// `Some(job)` when a job was claimed, otherwise `None`.
    fn steal_worker_job(&self, worker_runtime: &WorkerRuntime) -> Option<PoolJob> {
        let queue_count = self.worker_queues.len();
        if queue_count <= 1 {
            return None;
        }
        let worker_index = worker_runtime.worker_index();
        let start = worker_runtime.next_steal_start(queue_count);
        for offset in 0..queue_count {
            let victim = &self.worker_queues[(start + offset) % queue_count];
            if victim.worker_index() == worker_index {
                continue;
            }
            if !victim.is_active() {
                continue;
            }
            if let Some(job) = victim.steal_into(&worker_runtime.local) {
                if !worker_runtime.local.is_empty() {
                    self.state.notify_one();
                }
                return self.accept_claimed_job(job, worker_runtime);
            }
        }
        None
    }

    /// Returns whether this pool should use worker-local queues.
    ///
    /// # Returns
    ///
    /// `true` for small fixed pools where local batching reduces global queue
    /// contention; `false` for larger pools where inbox routing and victim
    /// scans cost more than they save.
    fn use_worker_local_queues(&self) -> bool {
        self.pool_size <= LOCAL_QUEUE_WORKER_LIMIT
    }

    /// Accepts a claimed queued job or cancels it after immediate shutdown.
    ///
    /// # Parameters
    ///
    /// * `job` - Job claimed from a queue.
    /// * `worker_runtime` - Queue runtime drained if stopping.
    ///
    /// # Returns
    ///
    /// `Some(job)` when the job may run, otherwise `None`.
    fn accept_claimed_job(&self, job: PoolJob, worker_runtime: &WorkerRuntime) -> Option<PoolJob> {
        if self.stop_now.load(Ordering::Acquire) {
            self.cancel_claimed_job(job);
            self.cancel_worker_jobs(worker_runtime);
            return None;
        }
        self.mark_queued_job_running();
        Some(job)
    }

    /// Cancels all jobs remaining in one worker runtime.
    ///
    /// # Parameters
    ///
    /// * `worker_runtime` - Worker-owned runtime to drain.
    fn cancel_worker_jobs(&self, worker_runtime: &WorkerRuntime) {
        while let Some(job) = worker_runtime.local.pop() {
            self.cancel_claimed_job(job);
        }
        for job in worker_runtime.queue.drain() {
            self.cancel_claimed_job(job);
        }
    }

    /// Marks one claimed queued job as running.
    fn mark_queued_job_running(&self) {
        let previous = self.queued_task_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "fixed pool queued counter underflow");
        self.running_task_count.fetch_add(1, Ordering::AcqRel);
    }

    /// Cancels one job claimed after immediate shutdown started.
    ///
    /// # Parameters
    ///
    /// * `job` - Queued job that must not be run.
    fn cancel_claimed_job(&self, job: PoolJob) {
        let previous = self.queued_task_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "fixed pool queued counter underflow");
        self.cancelled_task_count.fetch_add(1, Ordering::Relaxed);
        job.cancel();
        self.state.notify_all();
    }

    /// Marks one running job as finished.
    fn finish_running_job(&self) {
        let previous = self.running_task_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "fixed pool running counter underflow");
        self.completed_task_count.fetch_add(1, Ordering::Relaxed);
        if previous == 1 && self.queued_count() == 0 {
            self.state.notify_all();
        }
    }

    /// Reserves one worker slot before spawning a worker thread.
    pub(crate) fn reserve_worker_slot(&self) {
        self.state.write(|state| {
            state.live_workers += 1;
        });
    }

    /// Rolls back one worker slot after spawn failure.
    pub(crate) fn rollback_worker_slot(&self) {
        self.state.write(|state| {
            state.live_workers = state
                .live_workers
                .checked_sub(1)
                .expect("fixed pool live worker counter underflow");
        });
    }

    /// Stops the pool after a build-time worker spawn failure.
    pub(crate) fn stop_after_failed_build(&self) {
        self.accepting.store(false, Ordering::Release);
        self.stop_now.store(true, Ordering::Release);
        self.state.write(|state| {
            state.lifecycle = FixedThreadPoolLifecycle::Stopping;
        });
        self.state.notify_all();
    }

    /// Blocks until the pool is fully terminated.
    fn wait_for_termination(&self) {
        self.state
            .wait_until(|state| self.is_terminated_locked(state), |_| ());
    }

    /// Requests graceful shutdown.
    fn shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        self.state.write(|state| {
            if state.lifecycle.is_running() {
                state.lifecycle = FixedThreadPoolLifecycle::Shutdown;
            }
        });
        self.state.notify_all();
    }

    /// Requests immediate shutdown and cancels visible queued jobs.
    ///
    /// # Returns
    ///
    /// Count-based shutdown report.
    fn shutdown_now(&self) -> ShutdownReport {
        self.accepting.store(false, Ordering::Release);
        self.stop_now.store(true, Ordering::Release);
        let running = self.running_count();
        let mut state = self.state.lock();
        state.lifecycle = FixedThreadPoolLifecycle::Stopping;
        while self.inflight_count() > 0 {
            state = state.wait();
        }
        drop(state);
        let jobs = self.drain_visible_queued_jobs();
        let cancelled = jobs.len();
        for job in jobs {
            self.cancel_claimed_job(job);
        }
        self.state.notify_all();
        ShutdownReport::new(cancelled, running, cancelled)
    }

    /// Drains all jobs currently visible in global and worker-local queues.
    ///
    /// # Returns
    ///
    /// Drained queued jobs.
    fn drain_visible_queued_jobs(&self) -> Vec<PoolJob> {
        let mut jobs = Vec::new();
        loop {
            let previous_count = jobs.len();
            self.drain_global_queue(&mut jobs);
            self.drain_worker_queues(&mut jobs);
            if jobs.len() == previous_count {
                return jobs;
            }
        }
    }

    /// Drains visible jobs from the global injector.
    ///
    /// # Parameters
    ///
    /// * `jobs` - Destination for drained jobs.
    fn drain_global_queue(&self, jobs: &mut Vec<PoolJob>) {
        while let Some(job) = steal_one(&self.global_queue) {
            jobs.push(job);
        }
    }

    /// Drains visible jobs from all worker-local queues.
    ///
    /// # Parameters
    ///
    /// * `jobs` - Destination for drained jobs.
    fn drain_worker_queues(&self, jobs: &mut Vec<PoolJob>) {
        for queue in &self.worker_queues {
            jobs.extend(queue.drain());
        }
    }

    /// Returns whether shutdown has started.
    ///
    /// # Returns
    ///
    /// `true` when lifecycle is not running.
    fn is_shutdown(&self) -> bool {
        self.state.read(|state| !state.lifecycle.is_running())
    }

    /// Returns whether the pool is terminated.
    ///
    /// # Returns
    ///
    /// `true` after shutdown and after all workers and jobs are gone.
    fn is_terminated(&self) -> bool {
        self.state.read(|state| self.is_terminated_locked(state))
    }

    /// Checks termination against one locked state snapshot.
    ///
    /// # Parameters
    ///
    /// * `state` - Locked state snapshot.
    ///
    /// # Returns
    ///
    /// `true` when the pool is terminal.
    fn is_terminated_locked(&self, state: &FixedThreadPoolState) -> bool {
        !state.lifecycle.is_running()
            && state.live_workers == 0
            && self.queued_count() == 0
            && self.running_count() == 0
            && self.inflight_count() == 0
    }

    /// Returns a point-in-time stats snapshot.
    ///
    /// # Returns
    ///
    /// Snapshot using fixed pool size for both core and maximum sizes.
    fn stats(&self) -> ThreadPoolStats {
        let queued_tasks = self.queued_count();
        let running_tasks = self.running_count();
        let submitted_tasks = self.submitted_task_count.load(Ordering::Relaxed);
        let completed_tasks = self.completed_task_count.load(Ordering::Relaxed);
        let cancelled_tasks = self.cancelled_task_count.load(Ordering::Relaxed);
        self.state.read(|state| ThreadPoolStats {
            core_pool_size: self.pool_size,
            maximum_pool_size: self.pool_size,
            live_workers: state.live_workers,
            idle_workers: state.idle_workers,
            queued_tasks,
            running_tasks,
            submitted_tasks,
            completed_tasks,
            cancelled_tasks,
            shutdown: !state.lifecycle.is_running(),
            terminated: self.is_terminated_locked(state),
        })
    }
}

/// Submit guard that leaves in-flight accounting on drop.
struct FixedSubmitGuard<'a> {
    /// Pool whose in-flight counter was entered.
    inner: &'a FixedThreadPoolInner,
}

impl Drop for FixedSubmitGuard<'_> {
    /// Leaves submit accounting and wakes shutdown waiters if needed.
    fn drop(&mut self) {
        let previous = self
            .inner
            .inflight_submissions
            .fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "fixed pool submit counter underflow");
        if previous == 1 && !self.inner.accepting.load(Ordering::Acquire) {
            self.inner.state.notify_all();
        }
    }
}

/// Fixed-size thread pool implementing [`ExecutorService`].
///
/// `FixedThreadPool` prestarts a fixed number of worker threads and does not
/// support runtime pool-size changes. Use [`super::ThreadPool`] when dynamic
/// core/maximum sizes or keep-alive policies are required.
pub struct FixedThreadPool {
    /// Shared fixed pool state.
    inner: Arc<FixedThreadPoolInner>,
}

impl FixedThreadPool {
    /// Builds a fixed pool from validated builder options.
    ///
    /// # Parameters
    ///
    /// * `pool_size` - Number of workers to prestart.
    /// * `queue_capacity` - Optional maximum queued task count.
    /// * `thread_name_prefix` - Prefix used for worker thread names.
    /// * `stack_size` - Optional worker stack size.
    ///
    /// # Returns
    ///
    /// A fixed thread-pool handle with workers already started.
    ///
    /// # Errors
    ///
    /// Returns [`ThreadPoolBuildError`] when a worker thread cannot be spawned.
    pub(crate) fn build_with_options(
        pool_size: usize,
        queue_capacity: Option<usize>,
        thread_name_prefix: String,
        stack_size: Option<usize>,
    ) -> Result<Self, ThreadPoolBuildError> {
        let mut worker_runtimes = Vec::with_capacity(pool_size);
        let mut worker_queues = Vec::with_capacity(pool_size);
        for index in 0..pool_size {
            let worker_runtime = WorkerRuntime::new(index);
            worker_queues.push(Arc::clone(&worker_runtime.queue));
            worker_runtimes.push(worker_runtime);
        }
        let inner = Arc::new(FixedThreadPoolInner::new(
            pool_size,
            queue_capacity,
            worker_queues,
        ));
        for (index, worker_runtime) in worker_runtimes.into_iter().enumerate() {
            inner.reserve_worker_slot();
            let worker_inner = Arc::clone(&inner);
            let mut builder =
                std::thread::Builder::new().name(format!("{}-{}", thread_name_prefix, index));
            if let Some(stack_size) = stack_size {
                builder = builder.stack_size(stack_size);
            }
            if let Err(source) =
                builder.spawn(move || run_fixed_worker(worker_inner, worker_runtime))
            {
                inner.rollback_worker_slot();
                inner.stop_after_failed_build();
                return Err(ThreadPoolBuildError::SpawnWorker { index, source });
            }
        }
        Ok(Self { inner })
    }

    /// Creates a fixed thread pool with `pool_size` prestarted workers.
    ///
    /// # Parameters
    ///
    /// * `pool_size` - Number of worker threads.
    ///
    /// # Returns
    ///
    /// A fixed thread pool.
    ///
    /// # Errors
    ///
    /// Returns [`ThreadPoolBuildError`] if the worker count is zero or a worker
    /// cannot be spawned.
    pub fn new(pool_size: usize) -> Result<Self, ThreadPoolBuildError> {
        Self::builder().pool_size(pool_size).build()
    }

    /// Creates a fixed pool builder.
    ///
    /// # Returns
    ///
    /// Builder with CPU parallelism defaults.
    pub fn builder() -> FixedThreadPoolBuilder {
        FixedThreadPoolBuilder::new()
    }

    /// Returns the fixed worker count.
    ///
    /// # Returns
    ///
    /// Number of workers in this pool.
    pub fn pool_size(&self) -> usize {
        self.inner.pool_size()
    }

    /// Returns the queued task count.
    ///
    /// # Returns
    ///
    /// Number of accepted tasks waiting to run.
    pub fn queued_count(&self) -> usize {
        self.inner.queued_count()
    }

    /// Returns the running task count.
    ///
    /// # Returns
    ///
    /// Number of tasks currently held by workers.
    pub fn running_count(&self) -> usize {
        self.inner.running_count()
    }

    /// Returns the live worker count.
    ///
    /// # Returns
    ///
    /// Number of worker loops that have not exited.
    pub fn live_worker_count(&self) -> usize {
        self.inner.state.read(|state| state.live_workers)
    }

    /// Returns a point-in-time stats snapshot.
    ///
    /// # Returns
    ///
    /// Snapshot containing queue, worker, and lifecycle counters.
    pub fn stats(&self) -> ThreadPoolStats {
        self.inner.stats()
    }
}

impl Drop for FixedThreadPool {
    /// Requests graceful shutdown when the pool handle is dropped.
    fn drop(&mut self) {
        self.inner.shutdown();
    }
}

impl ExecutorService for FixedThreadPool {
    type Handle<R, E>
        = TaskHandle<R, E>
    where
        R: Send + 'static,
        E: Send + 'static;

    type Termination<'a>
        = Pin<Box<dyn Future<Output = ()> + Send + 'a>>
    where
        Self: 'a;

    /// Accepts a callable and queues it for fixed pool workers.
    ///
    /// # Parameters
    ///
    /// * `task` - Callable to execute on a fixed pool worker.
    ///
    /// # Returns
    ///
    /// A [`TaskHandle`] for the accepted task.
    ///
    /// # Errors
    ///
    /// Returns [`RejectedExecution::Shutdown`] after shutdown or
    /// [`RejectedExecution::Saturated`] when a bounded queue is full.
    fn submit_callable<C, R, E>(&self, task: C) -> Result<Self::Handle<R, E>, RejectedExecution>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let (handle, completion) = TaskCompletionPair::new().into_parts();
        let job = PoolJob::from_task(task, completion);
        self.inner.submit(job)?;
        Ok(handle)
    }

    /// Stops accepting new work and drains accepted queued tasks.
    fn shutdown(&self) {
        self.inner.shutdown();
    }

    /// Stops accepting work and cancels queued tasks.
    ///
    /// # Returns
    ///
    /// A count-based shutdown report.
    fn shutdown_now(&self) -> ShutdownReport {
        self.inner.shutdown_now()
    }

    /// Returns whether shutdown has been requested.
    ///
    /// # Returns
    ///
    /// `true` when this pool no longer accepts new work.
    fn is_shutdown(&self) -> bool {
        self.inner.is_shutdown()
    }

    /// Returns whether this pool is fully terminated.
    ///
    /// # Returns
    ///
    /// `true` after shutdown and after all workers have exited.
    fn is_terminated(&self) -> bool {
        self.inner.is_terminated()
    }

    /// Waits until this fixed pool has terminated.
    ///
    /// # Returns
    ///
    /// A future that blocks the polling thread until termination.
    fn await_termination(&self) -> Self::Termination<'_> {
        Box::pin(async move {
            self.inner.wait_for_termination();
        })
    }
}

/// Runs one fixed-pool worker loop.
///
/// # Parameters
///
/// * `inner` - Shared fixed-pool state.
/// * `worker_runtime` - Queue runtime owned by this worker.
fn run_fixed_worker(inner: Arc<FixedThreadPoolInner>, worker_runtime: WorkerRuntime) {
    worker_runtime.queue.activate();
    loop {
        if let Some(job) = inner.try_take_job(&worker_runtime) {
            job.run();
            inner.finish_running_job();
            continue;
        }
        if !wait_for_fixed_pool_work(&inner) {
            break;
        }
    }
    worker_exited(&inner, &worker_runtime.queue);
}

/// Waits until visible work exists or the worker should exit.
///
/// # Parameters
///
/// * `inner` - Shared fixed-pool state.
///
/// # Returns
///
/// `true` when the worker should try to take work again, or `false` when it
/// should exit.
fn wait_for_fixed_pool_work(inner: &FixedThreadPoolInner) -> bool {
    let mut state = inner.state.lock();
    loop {
        match state.lifecycle {
            FixedThreadPoolLifecycle::Running => {
                if inner.queued_count() > 0 {
                    return true;
                }
                mark_fixed_worker_idle(inner, &mut state);
                if inner.queued_count() > 0 || inner.has_pending_worker_wake() {
                    unmark_fixed_worker_idle(inner, &mut state);
                    return true;
                }
                state = state.wait();
                unmark_fixed_worker_idle(inner, &mut state);
            }
            FixedThreadPoolLifecycle::Shutdown => {
                if inner.queued_count() > 0 {
                    return true;
                }
                if inner.queued_count() == 0 && inner.inflight_count() == 0 {
                    return false;
                }
                mark_fixed_worker_idle(inner, &mut state);
                if inner.queued_count() > 0
                    || inner.inflight_count() == 0
                    || inner.has_pending_worker_wake()
                {
                    unmark_fixed_worker_idle(inner, &mut state);
                    continue;
                }
                state = state.wait();
                unmark_fixed_worker_idle(inner, &mut state);
            }
            FixedThreadPoolLifecycle::Stopping => return false,
        }
    }
}

/// Marks a fixed-pool worker as idle in locked and lock-free state.
///
/// # Parameters
///
/// * `inner` - Fixed pool whose idle counter is updated.
/// * `state` - Locked mutable state containing authoritative idle workers.
fn mark_fixed_worker_idle(inner: &FixedThreadPoolInner, state: &mut FixedThreadPoolState) {
    state.idle_workers += 1;
    inner.idle_worker_count.fetch_add(1, Ordering::AcqRel);
}

/// Marks a fixed-pool worker as no longer idle.
///
/// # Parameters
///
/// * `inner` - Fixed pool whose idle counter is updated.
/// * `state` - Locked mutable state containing authoritative idle workers.
fn unmark_fixed_worker_idle(inner: &FixedThreadPoolInner, state: &mut FixedThreadPoolState) {
    state.idle_workers = state
        .idle_workers
        .checked_sub(1)
        .expect("fixed pool idle worker counter underflow");
    let previous = inner.idle_worker_count.fetch_sub(1, Ordering::AcqRel);
    debug_assert!(previous > 0, "fixed pool idle worker counter underflow");
    inner.consume_pending_worker_wake();
}

/// Marks one fixed-pool worker as exited.
///
/// # Parameters
///
/// * `inner` - Shared fixed-pool state.
/// * `worker_queue` - Queue owned by the exiting worker.
fn worker_exited(inner: &FixedThreadPoolInner, worker_queue: &WorkerQueue) {
    worker_queue.deactivate();
    inner.state.write(|state| {
        state.live_workers = state
            .live_workers
            .checked_sub(1)
            .expect("fixed pool live worker counter underflow");
    });
    inner.state.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::thread;
    use std::time::Duration;

    fn counted_job(cancelled: Arc<AtomicUsize>, ran: Arc<AtomicUsize>) -> PoolJob {
        PoolJob::new(
            Box::new(move || {
                ran.fetch_add(1, Ordering::AcqRel);
            }),
            Box::new(move || {
                cancelled.fetch_add(1, Ordering::AcqRel);
            }),
        )
    }

    #[test]
    fn test_accept_claimed_job_stop_now_cancels_claimed_and_worker_queues() {
        let runtime = WorkerRuntime::new(0);
        runtime.queue.activate();
        let inner = FixedThreadPoolInner::new(1, None, vec![Arc::clone(&runtime.queue)]);
        inner.stop_now.store(true, Ordering::Release);

        let cancelled = Arc::new(AtomicUsize::new(0));
        let ran = Arc::new(AtomicUsize::new(0));
        runtime
            .local
            .push(counted_job(Arc::clone(&cancelled), Arc::clone(&ran)));
        runtime
            .queue
            .push_back(counted_job(Arc::clone(&cancelled), Arc::clone(&ran)));
        inner.queued_task_count.store(3, Ordering::Release);

        let accepted =
            inner.accept_claimed_job(counted_job(cancelled.clone(), ran.clone()), &runtime);
        assert!(accepted.is_none());
        assert_eq!(inner.queued_count(), 0);
        assert_eq!(inner.cancelled_task_count.load(Ordering::Acquire), 3);
        assert_eq!(cancelled.load(Ordering::Acquire), 3);
        assert_eq!(ran.load(Ordering::Acquire), 0);
    }

    #[test]
    fn test_steal_global_job_notifies_when_batch_leaves_local_jobs() {
        let runtime = WorkerRuntime::new(0);
        let inner = FixedThreadPoolInner::new(1, None, vec![Arc::clone(&runtime.queue)]);
        let cancelled = Arc::new(AtomicUsize::new(0));
        let ran = Arc::new(AtomicUsize::new(0));
        runtime
            .local
            .push(counted_job(Arc::clone(&cancelled), Arc::clone(&ran)));
        inner
            .global_queue
            .push(counted_job(Arc::clone(&cancelled), Arc::clone(&ran)));
        inner.queued_task_count.store(2, Ordering::Release);

        let claimed = inner
            .steal_global_job(&runtime)
            .expect("global queue should provide one claimed job");
        claimed.run();
        inner.finish_running_job();
        let remaining = runtime
            .local
            .pop()
            .expect("preloaded local job should remain queued");
        inner.cancel_claimed_job(remaining);

        assert_eq!(inner.queued_count(), 0);
        assert_eq!(inner.running_count(), 0);
        assert_eq!(inner.completed_task_count.load(Ordering::Acquire), 1);
        assert_eq!(inner.cancelled_task_count.load(Ordering::Acquire), 1);
        assert_eq!(ran.load(Ordering::Acquire), 1);
        assert_eq!(cancelled.load(Ordering::Acquire), 1);
    }

    #[test]
    fn test_steal_worker_job_notifies_when_batch_leaves_local_jobs() {
        let thief = WorkerRuntime::new(0);
        let victim = WorkerRuntime::new(1);
        thief.queue.activate();
        victim.queue.activate();
        let inner = FixedThreadPoolInner::new(
            2,
            None,
            vec![Arc::clone(&thief.queue), Arc::clone(&victim.queue)],
        );
        let cancelled = Arc::new(AtomicUsize::new(0));
        let ran = Arc::new(AtomicUsize::new(0));
        thief
            .local
            .push(counted_job(Arc::clone(&cancelled), Arc::clone(&ran)));
        victim
            .queue
            .push_back(counted_job(Arc::clone(&cancelled), Arc::clone(&ran)));
        inner.queued_task_count.store(2, Ordering::Release);

        let claimed = inner
            .steal_worker_job(&thief)
            .expect("victim queue should provide one claimed job");
        claimed.run();
        inner.finish_running_job();
        let remaining = thief
            .local
            .pop()
            .expect("batch steal should leave one local job");
        inner.cancel_claimed_job(remaining);

        assert_eq!(inner.queued_count(), 0);
        assert_eq!(inner.running_count(), 0);
        assert_eq!(inner.completed_task_count.load(Ordering::Acquire), 1);
        assert_eq!(inner.cancelled_task_count.load(Ordering::Acquire), 1);
        assert_eq!(ran.load(Ordering::Acquire), 1);
        assert_eq!(cancelled.load(Ordering::Acquire), 1);
    }

    #[test]
    fn test_fixed_submit_guard_drop_notifies_when_shutdown_closes_admission() {
        let runtime = WorkerRuntime::new(0);
        let inner = FixedThreadPoolInner::new(1, None, vec![Arc::clone(&runtime.queue)]);
        inner.inflight_submissions.store(1, Ordering::Release);
        inner.accepting.store(false, Ordering::Release);

        {
            let guard = FixedSubmitGuard { inner: &inner };
            drop(guard);
        }

        assert_eq!(inner.inflight_count(), 0);
    }

    #[test]
    fn test_wait_for_fixed_pool_work_shutdown_waits_for_inflight_submissions() {
        let runtime = WorkerRuntime::new(0);
        let inner = Arc::new(FixedThreadPoolInner::new(
            1,
            None,
            vec![Arc::clone(&runtime.queue)],
        ));
        inner.state.write(|state| {
            state.lifecycle = FixedThreadPoolLifecycle::Shutdown;
        });
        inner.inflight_submissions.store(1, Ordering::Release);
        inner.pending_worker_wakes.store(1, Ordering::Release);

        let inner_for_release = Arc::clone(&inner);
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            inner_for_release
                .inflight_submissions
                .store(0, Ordering::Release);
            inner_for_release.state.notify_all();
        });

        assert!(!wait_for_fixed_pool_work(&inner));
        releaser.join().expect("releaser thread should finish");
    }

    #[test]
    fn test_shutdown_now_waits_for_inflight_submissions() {
        let runtime = WorkerRuntime::new(0);
        let inner = Arc::new(FixedThreadPoolInner::new(
            1,
            None,
            vec![Arc::clone(&runtime.queue)],
        ));
        inner.inflight_submissions.store(1, Ordering::Release);

        let inner_for_release = Arc::clone(&inner);
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            inner_for_release
                .inflight_submissions
                .store(0, Ordering::Release);
            inner_for_release.state.notify_all();
        });

        let report = inner.shutdown_now();
        releaser.join().expect("releaser thread should finish");
        assert_eq!(report.running, 0);
        assert_eq!(report.queued, 0);
        assert_eq!(report.cancelled, 0);
    }
}
