// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

mod internal;

use crossbeam_deque::Injector;
use crossbeam_deque::Steal;
use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_executor::service::StopReport;
use qubit_executor::service::SubmissionError;
use qubit_lock::ParkingLotMonitor;

use self::internal::FixedSubmitGuard;
use super::fixed_thread_pool_state::FixedThreadPoolState;
use crate::PoolJob;
use crate::PoolJobSubmissionError;
use crate::ThreadPoolHooks;
use crate::ThreadPoolStats;
use crate::internal::AdmissionGate;

/// Shared state for a fixed-size thread pool.
pub struct FixedThreadPoolInner {
    /// Number of workers in this fixed pool.
    pub pool_size: usize,
    /// Mutable lifecycle and worker counters.
    pub state: ParkingLotMonitor<FixedThreadPoolState>,
    /// Admission gate used by submitters.
    pub admission: AdmissionGate,
    /// Whether immediate shutdown has requested workers to stop taking jobs.
    pub stop_now: AtomicBool,
    /// Submit calls that have passed the first admission check.
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
    /// Number of queued-job cancellation callbacks currently running.
    pub cancelling_task_count: AtomicUsize,
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
            admission: AdmissionGate::new(),
            stop_now: AtomicBool::new(false),
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
        self.admission.inflight_count()
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
        if self.admission.try_enter() {
            Ok(FixedSubmitGuard { inner: self })
        } else {
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

    /// Releases one reserved queue slot that never became runnable.
    fn release_queue_slot(&self) {
        let previous = self.queue_slot_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "fixed pool queue slot counter underflow");
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
    pub(crate) fn submit(&self, job: PoolJob) -> Result<(), PoolJobSubmissionError> {
        let _guard = self.begin_submit()?;
        if !self.reserve_queue_slot() {
            return Err(PoolJobSubmissionError::Rejected(SubmissionError::Saturated));
        }
        if job.accept().is_err() {
            self.release_queue_slot();
            self.notify_waiters_after_atomic_change();
            return Err(PoolJobSubmissionError::AcceptancePanicked);
        }
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
            self.state.lock().notify_one();
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
        self.state.lock().notify_all();
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
        self.begin_cancel_queued_job();
        job.cancel();
        self.finish_cancelled_job();
    }

    /// Moves one claimed queued job into cancellation callback execution.
    fn begin_cancel_queued_job(&self) {
        let previous = self.queued_task_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "fixed pool queued counter underflow");
        self.cancelling_task_count.fetch_add(1, Ordering::Release);
    }

    /// Completes one queued-job cancellation callback and releases its slot.
    fn finish_cancelled_job(&self) {
        let previous = self.cancelling_task_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "fixed pool cancelling task counter underflow");
        let previous = self.queue_slot_count.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "fixed pool queue slot counter underflow");
        self.cancelled_task_count.fetch_add(1, Ordering::Release);
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
        self.state.with_write(|state| {
            state.live_workers += 1;
        });
    }

    /// Rolls back one worker slot after spawn failure.
    pub fn rollback_worker_slot(&self) {
        self.state.with_write(|state| {
            state.live_workers = state
                .live_workers
                .checked_sub(1)
                .expect("fixed pool live worker counter underflow");
        });
    }

    /// Stops the pool after a build-time worker spawn failure.
    pub fn stop_after_failed_build(&self) {
        self.admission.close();
        self.stop_now.store(true, Ordering::Release);
        self.state.with_write_notify_all(|state| {
            state.lifecycle = ExecutorServiceLifecycle::Stopping;
        });
    }

    /// Blocks until the pool is fully terminated.
    pub fn wait_for_termination(&self) {
        self.state
            .wait_until_ready(|state| self.is_terminated_locked(state));
    }

    /// Waits for termination for at most `timeout`.
    pub fn wait_for_termination_timeout(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return self.is_terminated();
            }
            match self.state.wait_until_ready_with_total_timeout(
                remaining.min(Duration::from_secs(3600)),
                |state| self.is_terminated_locked(state),
            ) {
                Ok(result) if result.is_ready() => return true,
                Ok(_) => {}
                Err(_) => return self.is_terminated(),
            }
        }
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
            state.wait();
        }
        let previous = self.idle_waiter_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "fixed pool idle waiter counter underflow");
    }

    /// Requests graceful shutdown.
    pub fn shutdown(&self) {
        self.admission.close();
        let mut state = self.state.lock();
        if state.lifecycle == ExecutorServiceLifecycle::Running {
            state.lifecycle = ExecutorServiceLifecycle::ShuttingDown;
        }
        state.notify_all();
    }

    /// Requests immediate shutdown and cancels visible queued jobs.
    ///
    /// The returned [`StopReport`] uses `running` as an informational snapshot
    /// of [`Self::running_count`] taken while stop is being requested. That
    /// value is not used to decide which jobs are cancelled, whether
    /// workers should exit, or whether termination has been reached. Those
    /// decisions are driven by the `stop_now` flag, queue draining,
    /// worker-side cancellation, and the live queue/running counters
    /// observed by termination checks.
    ///
    /// Because fixed-pool workers claim jobs and update the running counter
    /// concurrently with callers of this method, the reported `running` value
    /// can be stale by the time the report is returned. A worker may finish
    /// just after the snapshot, or a narrow claim/accounting race may make
    /// a job appear as queued, running, or cancelled in a different
    /// snapshot. Callers must treat `StopReport::running` as monitoring
    /// data rather than as an exact synchronization guarantee.
    ///
    /// # Returns
    ///
    /// Count-based shutdown report. The queued and cancelled counts describe
    /// jobs drained by this stop request; the running count is only the
    /// best-effort snapshot described above.
    pub fn stop(&self) -> StopReport {
        let cancelled_before_stop = self.cancelled_task_count.load(Ordering::Acquire);
        let cancelling_before_stop = self.cancelling_task_count.load(Ordering::Acquire);
        self.admission.close();
        self.stop_now.store(true, Ordering::Release);
        let (jobs, queued, running) = {
            let mut state = self.state.lock();
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
                debug_assert!(previous > 0, "fixed pool submit waiter counter underflow");
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
        self.state.lock().notify_all();
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
            .with_read(|state| state.lifecycle != ExecutorServiceLifecycle::Running)
    }

    /// Returns the current lifecycle state.
    ///
    /// # Returns
    ///
    /// [`ExecutorServiceLifecycle::Terminated`] after all accepted work and
    /// workers are gone, otherwise the stored lifecycle state.
    pub fn lifecycle(&self) -> ExecutorServiceLifecycle {
        self.state.with_read(|state| {
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
        self.state
            .with_read(|state| self.is_terminated_locked(state))
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
            && self.cancelling_task_count.load(Ordering::Acquire) == 0
            && self.inflight_count() == 0
    }

    /// Checks whether all accepted work has drained.
    ///
    /// # Returns
    ///
    /// `true` when no submitter is still admitting work, no queued slot
    /// remains, and no worker-held task is running.
    fn is_idle_locked(&self) -> bool {
        self.queue_slot_count.load(Ordering::Acquire) == 0
            && self.running_count() == 0
            && self.cancelling_task_count.load(Ordering::Acquire) == 0
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
        self.state.with_read(|state| ThreadPoolStats {
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
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;

    use super::FixedThreadPoolInner;
    use crate::PoolJob;
    use crate::ThreadPoolHooks;

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
        assert!(inner.admission.try_enter());
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
        assert!(inner.admission.leave() || !inner.admission.is_open());
        inner.notify_waiters_after_atomic_change();
        let report = stop_thread
            .join()
            .expect("stop caller should not panic while building report");

        assert_eq!(report.queued, 1);
        assert_eq!(report.running, 0);
        assert_eq!(report.cancelled, 1);
        assert_eq!(inner.cancelled_task_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn test_fixed_stop_waits_for_cancel_callback() {
        let inner = Arc::new(FixedThreadPoolInner::with_hooks(
            1,
            None,
            ThreadPoolHooks::new(),
        ));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        inner
            .submit(PoolJob::new(
                Box::new(|| panic!("cancelled job must not run")),
                Box::new(move || {
                    entered_tx.send(()).expect("cancel callback should start");
                    release_rx
                        .recv()
                        .expect("cancel callback should be released");
                }),
            ))
            .expect("job should be accepted");

        let stop_inner = Arc::clone(&inner);
        let stop_thread = thread::spawn(move || stop_inner.stop());
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stop should enter the cancellation callback");

        assert!(!inner.is_terminated());
        let idle_inner = Arc::clone(&inner);
        let (idle_done_tx, idle_done_rx) = mpsc::channel();
        let idle_thread = thread::spawn(move || {
            idle_inner.wait_until_idle();
            idle_done_tx
                .send(())
                .expect("idle wait result should be reported");
        });
        let termination_inner = Arc::clone(&inner);
        let (termination_done_tx, termination_done_rx) = mpsc::channel();
        let termination_thread = thread::spawn(move || {
            termination_inner.wait_for_termination();
            termination_done_tx
                .send(())
                .expect("termination wait result should be reported");
        });
        assert!(
            idle_done_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
        assert!(
            termination_done_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
        assert!(!inner.is_terminated());

        release_tx
            .send(())
            .expect("cancel callback should be released");
        let report = stop_thread.join().expect("stop should finish");
        idle_thread
            .join()
            .expect("idle wait should finish after cancellation");
        termination_thread
            .join()
            .expect("termination wait should finish after cancellation");
        assert_eq!(report.cancelled, 1);
        assert_eq!(inner.cancelled_task_count.load(Ordering::Acquire), 1);
        assert!(inner.is_terminated());
    }

    #[test]
    fn test_fixed_accept_panic_drops_job_without_leaking_state() {
        let inner = FixedThreadPoolInner::with_hooks(1, None, ThreadPoolHooks::new());
        let ran = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicUsize::new(0));
        let ran_for_job = Arc::clone(&ran);
        let cancelled_for_job = Arc::clone(&cancelled);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            inner.submit(PoolJob::with_accept(
                Box::new(|| panic!("accept callback should be contained")),
                Box::new(move || {
                    ran_for_job.fetch_add(1, Ordering::Relaxed);
                }),
                Box::new(move || {
                    cancelled_for_job.fetch_add(1, Ordering::Relaxed);
                }),
            ))
        }));

        assert!(
            result.is_ok(),
            "accept callback panic must not escape submit"
        );
        assert!(matches!(
            result.expect("submit result should exist"),
            Err(crate::PoolJobSubmissionError::AcceptancePanicked)
        ));
        assert_eq!(inner.queued_count(), 0);
        assert_eq!(inner.queue_slot_count.load(Ordering::Acquire), 0);
        assert_eq!(inner.submitted_task_count.load(Ordering::Acquire), 0);
        assert_eq!(inner.completed_task_count.load(Ordering::Acquire), 0);
        assert_eq!(ran.load(Ordering::Relaxed), 0);
        assert_eq!(cancelled.load(Ordering::Relaxed), 0);
    }
}
