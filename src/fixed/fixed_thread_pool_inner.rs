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
use std::sync::atomic::{
    AtomicBool,
    AtomicUsize,
    Ordering,
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
use qubit_lock::ParkingLotMonitor;

use super::fixed_thread_pool_state::FixedThreadPoolState;
use crate::{
    PoolJob,
    ThreadPoolHooks,
    ThreadPoolStats,
};

/// Submit guard that leaves in-flight accounting on drop.
struct FixedSubmitGuard<'a> {
    /// Pool whose in-flight counter was entered.
    inner: &'a FixedThreadPoolInner,
}

impl Drop for FixedSubmitGuard<'_> {
    /// Leaves submit accounting and wakes waiters if needed.
    fn drop(&mut self) {
        let previous = self
            .inner
            .inflight_submissions
            .fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "fixed pool submit counter underflow");
        if previous == 1 && (self.inner.has_submit_waiters() || self.inner.has_idle_waiters()) {
            self.inner.notify_waiters_after_atomic_change();
        }
    }
}

/// Shared state for a fixed-size thread pool.
pub struct FixedThreadPoolInner {
    /// Number of workers in this fixed pool.
    pub pool_size: usize,
    /// Mutable lifecycle and worker counters.
    pub state: ParkingLotMonitor<FixedThreadPoolState>,
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
    /// Number of callers waiting for in-flight submitters to leave admission.
    pub submit_waiter_count: AtomicUsize,
    /// Number of callers waiting for accepted work to become idle.
    pub idle_waiter_count: AtomicUsize,
    /// Lock-free queue for externally submitted jobs.
    pub(crate) global_queue: Injector<PoolJob>,
    /// Optional maximum number of queued jobs.
    pub queue_capacity: Option<usize>,
    /// Number of reserved queue slots not yet started or cancelled.
    pub queue_slot_count: AtomicUsize,
    /// Number of jobs published to the queue and not yet started or cancelled.
    pub queued_task_count: AtomicUsize,
    /// Number of jobs currently running.
    pub running_task_count: AtomicUsize,
    /// Total number of accepted jobs.
    pub submitted_task_count: AtomicUsize,
    /// Total number of finished worker-held jobs.
    pub completed_task_count: AtomicUsize,
    /// Total number of queued jobs cancelled by immediate shutdown.
    pub cancelled_task_count: AtomicUsize,
    /// Worker and task lifecycle hooks.
    hooks: ThreadPoolHooks,
}

impl FixedThreadPoolInner {
    /// Creates shared state with explicit lifecycle hooks.
    ///
    /// # Parameters
    ///
    /// * `pool_size` - Number of workers that will be prestarted.
    /// * `queue_capacity` - Optional queue capacity.
    /// * `hooks` - Worker and task lifecycle hooks.
    ///
    /// # Returns
    ///
    /// A shared state object ready for worker startup.
    pub(crate) fn with_hooks(
        pool_size: usize,
        queue_capacity: Option<usize>,
        hooks: ThreadPoolHooks,
    ) -> Self {
        Self {
            pool_size,
            state: ParkingLotMonitor::new(FixedThreadPoolState::new()),
            accepting: AtomicBool::new(true),
            stop_now: AtomicBool::new(false),
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
            submitted_task_count: AtomicUsize::new(0),
            completed_task_count: AtomicUsize::new(0),
            cancelled_task_count: AtomicUsize::new(0),
            hooks,
        }
    }

    /// Returns the hook set used by worker threads.
    ///
    /// # Returns
    ///
    /// Worker and task lifecycle hooks.
    #[inline]
    pub fn hooks(&self) -> &ThreadPoolHooks {
        &self.hooks
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
    /// Returns [`SubmissionError::Shutdown`] when admission is closed.
    fn begin_submit(&self) -> Result<FixedSubmitGuard<'_>, SubmissionError> {
        self.inflight_submissions.fetch_add(1, Ordering::Release);
        if self.accepting.load(Ordering::Acquire) {
            Ok(FixedSubmitGuard { inner: self })
        } else {
            let previous = self.inflight_submissions.fetch_sub(1, Ordering::Release);
            debug_assert!(previous > 0, "fixed pool submit counter underflow");
            if previous == 1 && self.has_submit_waiters() {
                self.notify_waiters_after_atomic_change();
            }
            Err(SubmissionError::Shutdown)
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
                .queue_slot_count
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    (current < capacity).then_some(current + 1)
                })
                .is_ok();
        }
        self.queue_slot_count.fetch_add(1, Ordering::Release);
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
    /// Returns [`SubmissionError::Shutdown`] after shutdown or
    /// [`SubmissionError::Saturated`] when the bounded queue is full.
    pub(crate) fn submit(&self, job: PoolJob) -> Result<(), SubmissionError> {
        let _guard = self.begin_submit()?;
        if !self.reserve_queue_slot() {
            return Err(SubmissionError::Saturated);
        }
        job.accept();
        self.submitted_task_count.fetch_add(1, Ordering::Release);
        self.enqueue_job(job);
        Ok(())
    }

    /// Enqueues one accepted job to a worker inbox or the global fallback.
    ///
    /// # Parameters
    ///
    /// * `job` - Job whose queued slot has already been reserved.
    fn enqueue_job(&self, job: PoolJob) {
        self.queued_task_count.fetch_add(1, Ordering::Release);
        self.global_queue.push(job);
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
            let _state = self.state.lock();
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

    /// Returns whether any caller is waiting for submit admission to drain.
    ///
    /// # Returns
    ///
    /// `true` when a waiter needs notification after the last in-flight submit
    /// leaves admission.
    pub fn has_submit_waiters(&self) -> bool {
        self.submit_waiter_count.load(Ordering::Acquire) > 0
    }

    /// Returns whether any caller is waiting for accepted work to drain.
    ///
    /// # Returns
    ///
    /// `true` when an idle waiter may need notification after an in-flight
    /// submitter leaves admission.
    pub fn has_idle_waiters(&self) -> bool {
        self.idle_waiter_count.load(Ordering::Acquire) > 0
    }

    /// Notifies monitor waiters after an atomic-only condition change.
    ///
    /// Fixed-pool queue and running counters are atomics, not fields protected
    /// by the lifecycle monitor. Taking the monitor lock before notifying
    /// closes the condition-variable lost-wakeup window for waiters that check
    /// those atomic predicates while holding the same monitor.
    pub fn notify_waiters_after_atomic_change(&self) {
        let _state = self.state.lock();
        self.state.notify_all();
    }

    /// Attempts to claim one queued job for a worker.
    ///
    /// # Returns
    ///
    /// `Some(job)` when a job was claimed, otherwise `None`.
    pub(crate) fn try_take_job(&self) -> Option<PoolJob> {
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
    /// * `job` - Job claimed from a queue.
    /// # Returns
    ///
    /// `Some(job)` when the job may run, otherwise `None`.
    pub(crate) fn accept_claimed_job(&self, job: PoolJob) -> Option<PoolJob> {
        if self.stop_now.load(Ordering::Acquire) {
            self.cancel_claimed_job(job);
            return None;
        }
        self.mark_queued_job_running();
        Some(job)
    }

    /// Marks one claimed queued job as running.
    fn mark_queued_job_running(&self) {
        let previous = self.queued_task_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "fixed pool queued counter underflow");
        let previous = self.queue_slot_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "fixed pool queue slot counter underflow");
        self.running_task_count.fetch_add(1, Ordering::Release);
    }

    /// Cancels one job claimed after immediate shutdown started.
    ///
    /// # Parameters
    ///
    /// * `job` - Queued job that must not be run.
    pub(crate) fn cancel_claimed_job(&self, job: PoolJob) {
        let previous = self.queued_task_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "fixed pool queued counter underflow");
        let previous = self.queue_slot_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "fixed pool queue slot counter underflow");
        self.cancelled_task_count.fetch_add(1, Ordering::Release);
        job.cancel();
        self.notify_waiters_after_atomic_change();
    }

    /// Marks one running job as finished.
    pub fn finish_running_job(&self) {
        let previous = self.running_task_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "fixed pool running counter underflow");
        self.completed_task_count.fetch_add(1, Ordering::Release);
        if previous == 1 && self.queued_count() == 0 {
            self.notify_waiters_after_atomic_change();
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
            state.lifecycle = ExecutorServiceLifecycle::Stopping;
        });
        self.state.notify_all();
    }

    /// Blocks until the pool is fully terminated.
    pub fn wait_for_termination(&self) {
        self.state
            .wait_until(|state| self.is_terminated_locked(state), |_| ());
    }

    /// Blocks until all accepted work has completed or been cancelled.
    ///
    /// This method waits for in-flight submissions, queued tasks, and running
    /// tasks to drain. It does not request shutdown and does not wait for fixed
    /// worker threads to exit.
    pub fn wait_until_idle(&self) {
        self.idle_waiter_count.fetch_add(1, Ordering::AcqRel);
        let mut state = self.state.lock();
        while !self.is_idle_locked() {
            state = state.wait();
        }
        let previous = self.idle_waiter_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "fixed pool idle waiter counter underflow");
    }

    /// Requests graceful shutdown.
    pub fn shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        self.submit_waiter_count.fetch_add(1, Ordering::AcqRel);
        let mut state = self.state.lock();
        while self.inflight_count() > 0 {
            state = state.wait();
        }
        let previous = self.submit_waiter_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "fixed pool submit waiter counter underflow");
        if state.lifecycle == ExecutorServiceLifecycle::Running {
            state.lifecycle = ExecutorServiceLifecycle::ShuttingDown;
        }
        drop(state);
        self.state.notify_all();
    }

    /// Requests immediate shutdown and cancels visible queued jobs.
    ///
    /// The returned [`StopReport`] uses `running` as an informational snapshot of
    /// [`Self::running_count`] taken while stop is being requested. That value is
    /// not used to decide which jobs are cancelled, whether workers should exit,
    /// or whether termination has been reached. Those decisions are driven by the
    /// `stop_now` flag, queue draining, worker-side cancellation, and the live
    /// queue/running counters observed by termination checks.
    ///
    /// Because fixed-pool workers claim jobs and update the running counter
    /// concurrently with callers of this method, the reported `running` value can
    /// be stale by the time the report is returned. A worker may finish just
    /// after the snapshot, or a narrow claim/accounting race may make a job appear
    /// as queued, running, or cancelled in a different snapshot. Callers must
    /// treat `StopReport::running` as monitoring data rather than as an exact
    /// synchronization guarantee.
    ///
    /// # Returns
    ///
    /// Count-based shutdown report. The queued and cancelled counts describe jobs
    /// drained by this stop request; the running count is only the best-effort
    /// snapshot described above.
    pub fn stop(&self) -> StopReport {
        let cancelled_before_stop = self.cancelled_task_count.load(Ordering::Acquire);
        self.accepting.store(false, Ordering::Release);
        self.stop_now.store(true, Ordering::Release);
        let mut state = self.state.lock();
        state.lifecycle = ExecutorServiceLifecycle::Stopping;
        if self.inflight_count() > 0 {
            self.submit_waiter_count.fetch_add(1, Ordering::AcqRel);
            while self.inflight_count() > 0 {
                state = state.wait();
            }
            let previous = self.submit_waiter_count.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "fixed pool submit waiter counter underflow");
        }
        // These snapshots are report-only. Workers may concurrently move jobs
        // between queued, running, and cancelled states, while actual
        // cancellation/termination relies on stop_now plus the live counters.
        let running = self.running_count();
        let worker_cancelled = self
            .cancelled_task_count
            .load(Ordering::Acquire)
            .saturating_sub(cancelled_before_stop);
        let queued = self.queued_count() + worker_cancelled;
        drop(state);
        let jobs = self.drain_visible_queued_jobs();
        for job in jobs {
            self.cancel_claimed_job(job);
        }
        self.state.notify_all();
        StopReport::new(queued, running, queued)
    }

    /// Drains all jobs currently visible in the global queue.
    ///
    /// # Returns
    ///
    /// Drained queued jobs.
    fn drain_visible_queued_jobs(&self) -> Vec<PoolJob> {
        let mut jobs = Vec::new();
        self.drain_global_queue(&mut jobs);
        jobs
    }

    /// Drains visible jobs from the global injector.
    ///
    /// # Parameters
    ///
    /// * `jobs` - Destination for drained jobs.
    fn drain_global_queue(&self, jobs: &mut Vec<PoolJob>) {
        while let Some(job) = Self::steal_one(&self.global_queue) {
            jobs.push(job);
        }
    }

    /// Returns whether shutdown has started.
    ///
    /// # Returns
    ///
    /// `true` when lifecycle is not running.
    pub fn is_not_running(&self) -> bool {
        self.state
            .read(|state| state.lifecycle != ExecutorServiceLifecycle::Running)
    }

    /// Returns the current lifecycle state.
    ///
    /// # Returns
    ///
    /// [`ExecutorServiceLifecycle::Terminated`] after all accepted work and
    /// workers are gone, otherwise the stored lifecycle state.
    pub fn lifecycle(&self) -> ExecutorServiceLifecycle {
        self.state.read(|state| {
            if self.is_terminated_locked(state) {
                ExecutorServiceLifecycle::Terminated
            } else {
                state.lifecycle
            }
        })
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
        state.lifecycle != ExecutorServiceLifecycle::Running
            && state.live_workers == 0
            && self.queue_slot_count.load(Ordering::Acquire) == 0
            && self.running_count() == 0
            && self.inflight_count() == 0
    }

    /// Checks whether all accepted work has drained.
    ///
    /// # Returns
    ///
    /// `true` when no submitter is still admitting work, no queued slot remains,
    /// and no worker-held task is running.
    fn is_idle_locked(&self) -> bool {
        self.queue_slot_count.load(Ordering::Acquire) == 0
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
            lifecycle: if self.is_terminated_locked(state) {
                ExecutorServiceLifecycle::Terminated
            } else {
                state.lifecycle
            },
            core_pool_size: self.pool_size,
            maximum_pool_size: self.pool_size,
            live_workers: state.live_workers,
            idle_workers: state.idle_workers,
            queued_tasks,
            running_tasks,
            submitted_tasks,
            completed_tasks,
            cancelled_tasks,
            terminated: self.is_terminated_locked(state),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::Ordering,
            mpsc,
        },
        thread,
        time::{
            Duration,
            Instant,
        },
    };

    use super::FixedThreadPoolInner;
    use crate::{
        PoolJob,
        ThreadPoolHooks,
    };

    fn wait_until<F>(mut condition: F)
    where
        F: FnMut() -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(condition(), "condition should become true within timeout");
    }

    /// Verifies the stop-report race where cancellation happens on the worker
    /// side after `stop_now` is published.
    ///
    /// The important interleaving is:
    ///
    /// 1. A job is accepted and visible as queued.
    /// 2. `stop()` publishes `stop_now`.
    /// 3. A worker that had already passed the outer `try_take_job()` stop
    ///    check claims the queued job, sees `stop_now` in
    ///    `accept_claimed_job()`, and cancels it.
    /// 4. `stop()` must still report that job as queued/cancelled even though
    ///    the global queue is now empty.
    ///
    /// The artificial in-flight submission below is only a synchronization
    /// gate: it keeps `stop()` waiting after step 2 so the test can
    /// deterministically exercise step 3 without relying on timing.
    #[test]
    fn test_stop_reports_worker_side_cancel_after_stop_now() {
        let inner = Arc::new(FixedThreadPoolInner::with_hooks(
            1,
            None,
            ThreadPoolHooks::new(),
        ));
        let (cancelled_tx, cancelled_rx) = mpsc::channel();

        inner
            .submit(PoolJob::new(
                Box::new(thread::yield_now),
                Box::new(move || {
                    cancelled_tx
                        .send(())
                        .expect("test should receive cancellation signal");
                }),
            ))
            .expect("job should be accepted before stop");
        assert_eq!(inner.queued_count(), 1);

        // Hold `stop()` between publishing `stop_now` and taking its report
        // snapshots. This simulates a submitter that had already crossed
        // admission before immediate shutdown began.
        inner.inflight_submissions.fetch_add(1, Ordering::Release);
        let stop_inner = Arc::clone(&inner);
        let stop_thread = thread::spawn(move || stop_inner.stop());
        wait_until(|| {
            inner.stop_now.load(Ordering::Acquire)
                && inner.submit_waiter_count.load(Ordering::Acquire) > 0
        });

        // This is the worker-side cancellation window being protected. The
        // public worker helper checks `stop_now` before stealing, so this test
        // directly models the lower-level interleaving where a worker passed
        // that check before `stop_now`, then claims the job afterwards.
        let job = FixedThreadPoolInner::steal_one(&inner.global_queue)
            .expect("queued job should remain visible while stop is gated");
        assert!(
            inner.accept_claimed_job(job).is_none(),
            "job claimed after stop_now should be cancelled by worker path",
        );
        cancelled_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker-side cancellation should publish cancellation signal");

        // Let `stop()` continue. The report must include the cancellation that
        // happened above, even though the global queue is already empty.
        let previous = inner.inflight_submissions.fetch_sub(1, Ordering::Release);
        assert_eq!(previous, 1);
        inner.notify_waiters_after_atomic_change();
        let report = stop_thread
            .join()
            .expect("stop caller should not panic while building report");

        assert_eq!(report.queued, 1);
        assert_eq!(report.running, 0);
        assert_eq!(report.cancelled, 1);
        assert_eq!(inner.cancelled_task_count.load(Ordering::Acquire), 1);
    }
}
