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
    Arc,
    atomic::{
        AtomicBool,
        AtomicUsize,
        Ordering,
    },
};

use crossbeam_deque::Injector;
use qubit_executor::service::{
    RejectedExecution,
    ShutdownReport,
};
use qubit_lock::Monitor;

use super::fixed_submit_guard::FixedSubmitGuard;
use super::fixed_thread_pool_lifecycle::FixedThreadPoolLifecycle;
use super::fixed_thread_pool_state::FixedThreadPoolState;
use super::fixed_worker_queue::FixedWorkerQueue;
use super::fixed_worker_runtime::FixedWorkerRuntime;
use super::queue_steal_source::{
    steal_batch_and_pop,
    steal_one,
};
use crate::{
    PoolJob,
    ThreadPoolStats,
};

/// Maximum number of worker-local queues probed by one submit call.
const LOCAL_ENQUEUE_MAX_PROBES: usize = 4;
/// Maximum worker count that uses worker-local batch queues.
const LOCAL_QUEUE_WORKER_LIMIT: usize = 4;
/// Shared state for a fixed-size thread pool.
pub struct FixedThreadPoolInner {
    /// Number of workers in this fixed pool.
    pub pool_size: usize,
    /// Mutable lifecycle and worker counters.
    pub state: Monitor<FixedThreadPoolState>,
    /// Admission gate used by submitters.
    pub accepting: AtomicBool,
    /// Whether immediate shutdown has requested workers to stop taking jobs.
    pub stop_now: AtomicBool,
    /// Submit calls that have passed the first admission check.
    pub inflight_submissions: AtomicUsize,
    /// Number of workers currently blocked or about to block waiting for work.
    pub idle_worker_count: AtomicUsize,
    /// Number of idle-worker wakeups already requested but not yet consumed.
    pub pending_worker_wakes: AtomicUsize,
    /// Lock-free queue for externally submitted jobs.
    pub global_queue: Injector<PoolJob>,
    /// Worker-local queues used for submit routing and work stealing.
    pub worker_queues: Vec<Arc<FixedWorkerQueue>>,
    /// Round-robin cursor used for submit-path local queue selection.
    pub next_enqueue_worker: AtomicUsize,
    /// Optional maximum number of queued jobs.
    pub queue_capacity: Option<usize>,
    /// Number of queued jobs not yet started or cancelled.
    pub queued_task_count: AtomicUsize,
    /// Number of jobs currently running.
    pub running_task_count: AtomicUsize,
    /// Total number of accepted jobs.
    pub submitted_task_count: AtomicUsize,
    /// Total number of finished worker-held jobs.
    pub completed_task_count: AtomicUsize,
    /// Total number of queued jobs cancelled by immediate shutdown.
    pub cancelled_task_count: AtomicUsize,
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
    pub fn new(
        pool_size: usize,
        queue_capacity: Option<usize>,
        worker_queues: Vec<Arc<FixedWorkerQueue>>,
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
    pub fn pool_size(&self) -> usize {
        self.pool_size
    }

    /// Returns the queued task count.
    ///
    /// # Returns
    ///
    /// Number of accepted tasks waiting to run.
    #[inline]
    pub fn queued_count(&self) -> usize {
        self.queued_task_count.load(Ordering::Acquire)
    }

    /// Returns the running task count.
    ///
    /// # Returns
    ///
    /// Number of tasks currently held by workers.
    #[inline]
    pub fn running_count(&self) -> usize {
        self.running_task_count.load(Ordering::Acquire)
    }

    /// Returns the number of in-flight submit calls.
    ///
    /// # Returns
    ///
    /// Number of submit calls that may still publish or roll back a queued job.
    #[inline]
    pub fn inflight_count(&self) -> usize {
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
            return self
                .queued_task_count
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    (current < capacity).then_some(current + 1)
                })
                .is_ok();
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
    pub fn submit(&self, job: PoolJob) -> Result<(), RejectedExecution> {
        let _guard = self.begin_submit()?;
        if !self.reserve_queue_slot() {
            return Err(RejectedExecution::Saturated);
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
            self.state.notify_one();
        }
    }

    /// Returns whether an idle-worker wakeup has been requested.
    ///
    /// # Returns
    ///
    /// `true` when at least one idle worker should leave the wait path and
    /// retry taking work.
    pub fn has_pending_worker_wake(&self) -> bool {
        self.pending_worker_wakes.load(Ordering::Acquire) > 0
    }

    /// Consumes one requested idle-worker wakeup if one exists.
    pub fn consume_pending_worker_wake(&self) {
        let _ = self.pending_worker_wakes.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| current.checked_sub(1),
        );
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
    pub fn try_take_job(&self, worker_runtime: &FixedWorkerRuntime) -> Option<PoolJob> {
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
    fn steal_global_job(&self, worker_runtime: &FixedWorkerRuntime) -> Option<PoolJob> {
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
    fn steal_single_global_job(&self, worker_runtime: &FixedWorkerRuntime) -> Option<PoolJob> {
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
    fn steal_worker_job(&self, worker_runtime: &FixedWorkerRuntime) -> Option<PoolJob> {
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
    pub fn accept_claimed_job(
        &self,
        job: PoolJob,
        worker_runtime: &FixedWorkerRuntime,
    ) -> Option<PoolJob> {
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
    fn cancel_worker_jobs(&self, worker_runtime: &FixedWorkerRuntime) {
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
    pub fn cancel_claimed_job(&self, job: PoolJob) {
        let previous = self.queued_task_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "fixed pool queued counter underflow");
        self.cancelled_task_count.fetch_add(1, Ordering::Relaxed);
        job.cancel();
        self.state.notify_all();
    }

    /// Marks one running job as finished.
    pub fn finish_running_job(&self) {
        let previous = self.running_task_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "fixed pool running counter underflow");
        self.completed_task_count.fetch_add(1, Ordering::Relaxed);
        if previous == 1 && self.queued_count() == 0 {
            self.state.notify_all();
        }
    }

    /// Reserves one worker slot before spawning a worker thread.
    pub fn reserve_worker_slot(&self) {
        self.state.write(|state| {
            state.live_workers += 1;
        });
    }

    /// Rolls back one worker slot after spawn failure.
    pub fn rollback_worker_slot(&self) {
        self.state.write(|state| {
            state.live_workers = state
                .live_workers
                .checked_sub(1)
                .expect("fixed pool live worker counter underflow");
        });
    }

    /// Stops the pool after a build-time worker spawn failure.
    pub fn stop_after_failed_build(&self) {
        self.accepting.store(false, Ordering::Release);
        self.stop_now.store(true, Ordering::Release);
        self.state.write(|state| {
            state.lifecycle = FixedThreadPoolLifecycle::Stopping;
        });
        self.state.notify_all();
    }

    /// Blocks until the pool is fully terminated.
    pub fn wait_for_termination(&self) {
        self.state
            .wait_until(|state| self.is_terminated_locked(state), |_| ());
    }

    /// Requests graceful shutdown.
    pub fn shutdown(&self) {
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
    pub fn shutdown_now(&self) -> ShutdownReport {
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
    pub fn is_shutdown(&self) -> bool {
        self.state.read(|state| !state.lifecycle.is_running())
    }

    /// Returns whether the pool is terminated.
    ///
    /// # Returns
    ///
    /// `true` after shutdown and after all workers and jobs are gone.
    pub fn is_terminated(&self) -> bool {
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
    pub fn stats(&self) -> ThreadPoolStats {
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
