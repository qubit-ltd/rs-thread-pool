// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Arc;
use std::time::Duration;

use qubit_executor::TaskHandle;
use qubit_executor::TrackedTask;
use qubit_executor::service::ExecutorService;
use qubit_executor::service::ExecutorServiceLifecycle;
use qubit_executor::service::RecoverableExecutorService;
use qubit_executor::service::RejectedTask;
use qubit_executor::service::StopReport;
use qubit_executor::service::SubmissionError;
use qubit_executor::task::spi::TaskEndpointPair;
use qubit_function::Callable;
use qubit_function::Runnable;

use super::thread_pool_builder::ThreadPoolBuilder;
use super::thread_pool_inner::ThreadPoolInner;
use crate::ExecutorServiceBuilderError;
use crate::PoolJob;
use crate::PoolJobSubmissionError;
use crate::PoolJobTicket;
use crate::ThreadPoolStats;

/// OS thread pool implementing [`ExecutorService`].
///
/// `ThreadPool` accepts fallible tasks, stores accepted waiting tasks in
/// internal queues, and executes them on worker threads. The global waiting
/// queue is FIFO, but task start and completion order are not a strict FIFO API
/// guarantee. Workers are created lazily up to the configured core size, tasks
/// are queued after that, and the pool may grow up to the maximum size when a
/// bounded queue is full. Callable submissions return [`TaskHandle`], while
/// tracked submissions return [`TrackedTask`].
///
/// `shutdown` is graceful: already accepted queued tasks are allowed to run.
/// `stop` is abrupt: it cancels queued tasks that have not been claimed by a
/// worker. A claimed task is not revoked, even if its user callback has not
/// begun. Queued tasks cancelled by stop complete with
/// [`TaskExecutionError::Cancelled`](qubit_executor::TaskExecutionError::Cancelled).
/// Dropping the pool requests graceful shutdown, but does not wait for accepted
/// work, cancellation callbacks, or workers to finish. When completion is
/// required, call [`ExecutorService::shutdown`] and
/// [`ExecutorService::wait_termination`] before dropping the pool, from outside
/// the pool. With the `async-wait` feature, `await_termination()` can be
/// awaited instead. A task or callback on this pool must not wait for
/// termination that depends on its own progress.
///
/// # Examples
///
/// ```
/// use qubit_executor::service::ExecutorService;
/// use qubit_thread_pool::ThreadPool;
///
/// let pool = ThreadPool::new(1)?;
/// pool.shutdown();
/// pool.wait_termination();
/// # Ok::<(), qubit_executor::service::ExecutorServiceBuilderError>(())
/// ```
pub struct ThreadPool {
    /// Shared pool state and worker coordination primitives.
    inner: Arc<ThreadPoolInner>,
}

impl ThreadPool {
    /// Returns this pool's private deterministic test scheduler.
    #[cfg(test)]
    pub(crate) fn notification_control(
        &self,
    ) -> Arc<crate::tests::notification_control::NotificationControl> {
        self.inner.notification_control()
    }

    /// Creates a thread pool with equal core and maximum pool sizes.
    ///
    /// # Parameters
    ///
    /// * `pool_size` - Value applied as both core and maximum pool size.
    ///
    /// # Returns
    ///
    /// `Ok(ThreadPool)` after validating the configuration. Workers are
    /// created lazily by the default builder, so the first submission may
    /// report [`SubmissionError::WorkerSpawnFailed`] if a worker cannot start.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorServiceBuilderError`] if the resulting maximum pool
    /// size is zero or another builder option is invalid.
    #[inline]
    pub fn new(pool_size: usize) -> Result<Self, ExecutorServiceBuilderError> {
        Self::builder().pool_size(pool_size).build()
    }

    /// Creates a builder for configuring a thread pool.
    ///
    /// # Returns
    ///
    /// A builder with default core and maximum pool sizes and an unbounded
    /// queue.
    #[inline]
    pub fn builder() -> ThreadPoolBuilder {
        ThreadPoolBuilder::default()
    }

    /// Wraps initialized shared state in the public pool handle.
    ///
    /// This constructor is restricted to the validated builder path.
    ///
    /// # Parameters
    ///
    /// * `inner` - Initialized shared pool state.
    pub(super) fn from_inner(inner: Arc<ThreadPoolInner>) -> Self {
        Self { inner }
    }

    /// Waits asynchronously until shutdown or stop has finished all work and
    /// every worker's `after_worker_stop` hook has returned.
    ///
    /// The wait does not occupy a worker or Tokio blocking thread. Dropping
    /// the future cancels only this wait, without changing the pool lifecycle.
    /// Completion does not guarantee that the operating system has physically
    /// reclaimed every worker thread.
    #[cfg(feature = "async-wait")]
    pub async fn await_termination(&self) {
        let mut receiver = self.inner.subscribe_termination();
        receiver
            .wait_for(|terminated| *terminated)
            .await
            .expect("pool state outlives a borrowed termination wait");
    }

    /// Returns the number of queued tasks waiting for a worker.
    ///
    /// # Returns
    ///
    /// The number of accepted tasks that have not started yet.
    #[must_use]
    #[inline]
    pub fn queued_count(&self) -> usize {
        self.inner.queued_count()
    }

    /// Returns the number of tasks currently held by workers.
    ///
    /// # Returns
    ///
    /// The number of tasks that workers have taken from the queue and have not
    /// yet finished processing.
    #[must_use]
    #[inline]
    pub fn running_count(&self) -> usize {
        self.inner.running_count()
    }

    /// Returns how many workers remain registered for pool scheduling.
    ///
    /// # Returns
    ///
    /// The number of workers registered for scheduling. A worker is removed
    /// from this count before its `after_worker_stop` hook runs, so this count
    /// excludes workers in that hook. It is a runtime count and need not match
    /// configured [`Self::core_pool_size`] or [`Self::maximum_pool_size`].
    #[must_use]
    #[inline]
    pub fn live_worker_count(&self) -> usize {
        self.inner.read_state(|state| state.live_workers)
    }

    /// Returns the configured core pool size.
    ///
    /// # Returns
    ///
    /// The number of workers kept for normal load before tasks are queued.
    #[must_use]
    #[inline]
    pub fn core_pool_size(&self) -> usize {
        self.inner.read_state(|state| state.core_pool_size)
    }

    /// Returns the configured maximum pool size.
    ///
    /// # Returns
    ///
    /// The maximum number of worker threads this pool may create.
    #[must_use]
    #[inline]
    pub fn maximum_pool_size(&self) -> usize {
        self.inner.read_state(|state| state.maximum_pool_size)
    }

    /// Returns a best-effort snapshot of pool counters.
    ///
    /// # Returns
    ///
    /// Monitor-protected worker and lifecycle fields with independently loaded
    /// task counters. See [`ThreadPoolStats`] for synchronization limitations.
    #[must_use]
    #[inline]
    pub fn stats(&self) -> ThreadPoolStats {
        self.inner.stats()
    }

    /// Attempts to submit a callable and returns it unchanged if admission
    /// fails.
    ///
    /// A rejected callable was not called, cloned, or retained by the pool, so
    /// callers may retry it or choose another disposition. Success means the
    /// task was accepted; it does not guarantee execution or task success.
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
    pub fn try_submit_callable<C, R, E>(&self, task: C) -> Result<TaskHandle<R, E>, RejectedTask<C>>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        self.inner.try_submit_callable(task)
    }

    /// Subscribes to queue capacity and lifecycle changes, including successful
    /// runtime core and maximum size updates.
    ///
    /// A notification is advisory and does not reserve a slot. Increasing the
    /// maximum size can make a saturated submission admissible; callers must
    /// retry submission and handle saturation because another submitter may
    /// claim the available capacity.
    ///
    /// # Returns
    ///
    /// A receiver that observes changes which may allow a submission retry.
    #[cfg(feature = "async-wait")]
    #[inline]
    #[must_use]
    pub fn capacity_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.capacity_changes()
    }

    /// Prepares callbacks and a ticket that can remove the accepted queued job.
    ///
    /// Submit the returned job only with this pool's [`Self::submit_job`].
    /// Preparing does not reserve capacity or invoke any callback. `accept`
    /// runs after admission succeeds; `run` runs if a worker claims the job;
    /// `cancel` runs if its ticket or `stop` cancels it first. Callback panic
    /// handling follows [`PoolJob::with_accept`].
    ///
    /// Ticket cancellation during acceptance waits for its result. On success,
    /// the submitting thread executes `cancel` before publishing to the queue.
    /// All callbacks execute outside pool and queue locks. Acceptance and
    /// cancellation callbacks must not call `stop`, `join`, or
    /// `wait_termination` on this pool, since those may wait for the callback.
    ///
    /// # Parameters
    ///
    /// * `accept` - Callback invoked after admission succeeds.
    /// * `run` - Callback invoked when a worker claims the accepted job.
    /// * `cancel` - Callback invoked when queued cancellation wins.
    ///
    /// # Returns
    ///
    /// The unsubmitted job and a cloneable ticket. Successful ticket
    /// cancellation releases captures and queue capacity before returning.
    #[must_use = "the job must be submitted and the ticket retained for cancellation"]
    #[inline]
    pub fn prepare_cancellable_job(
        &self,
        accept: Box<dyn FnOnce() + Send + 'static>,
        run: Box<dyn FnOnce() + Send + 'static>,
        cancel: Box<dyn FnOnce() + Send + 'static>,
    ) -> (PoolJob, PoolJobTicket) {
        PoolJob::prepare_cancellable(&self.inner, accept, run, cancel)
    }

    /// Submits a custom pool job.
    ///
    /// This low-level extension point is intended for higher-level services
    /// that need to pair pool execution with their own task registry or
    /// cancellation bookkeeping. Prefer the [`ExecutorService`] submission
    /// methods for ordinary runnable and callable work. Custom job callbacks
    /// run synchronously on the thread that reaches the corresponding lifecycle
    /// event and should stay short and non-blocking. Callback panics are
    /// contained; if an acceptance callback panics, the job is not queued,
    /// run, or cancelled.
    ///
    /// Acceptance runs synchronously on the submitting thread after admission
    /// and any required worker creation succeeds. The callback may call
    /// `shutdown` on the same pool because shutdown closes admission
    /// without waiting. It must not call `stop`, `join`, or
    /// `wait_termination`, which may wait for this submission
    /// to leave admission. Non-blocking observation such as [`Self::stats`] is
    /// safe.
    ///
    /// # Parameters
    ///
    /// * `job` - Custom job to execute or cancel later.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the pool accepts the job.
    ///
    /// # Errors
    ///
    /// Returns [`PoolJobSubmissionError::Rejected`] when the executor rejects
    /// the job, or [`PoolJobSubmissionError::AcceptancePanicked`] when the
    /// acceptance callback panics. In the latter case the job is neither
    /// run nor cancelled.
    ///
    /// # Panics
    ///
    /// Panics if a ticketed job was prepared by a different dynamic pool.
    #[inline]
    pub fn submit_job(&self, job: PoolJob) -> Result<(), PoolJobSubmissionError> {
        self.inner.submit(job)
    }

    /// Blocks until all accepted work has completed.
    ///
    /// This is a join-style wait for quiescence: it does not request shutdown
    /// and does not wait for worker threads to exit. Concurrent submissions may
    /// extend the wait until those accepted jobs also drain.
    #[inline]
    pub fn join(&self) {
        self.inner.wait_until_idle();
    }

    /// Starts one core worker if the pool has fewer live workers than its
    /// configured core size.
    ///
    /// # Returns
    ///
    /// `Ok(true)` if a worker was started, or `Ok(false)` if no core worker
    /// was needed.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] if the pool is shut down, or
    /// [`SubmissionError::WorkerSpawnFailed`] if worker creation fails.
    #[inline]
    pub fn prestart_core_thread(&self) -> Result<bool, SubmissionError> {
        self.inner.prestart_core_thread()
    }

    /// Starts all missing core workers.
    ///
    /// # Returns
    ///
    /// The number of workers started.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] if the pool is shut down, or
    /// [`SubmissionError::WorkerSpawnFailed`] if worker creation fails.
    #[inline]
    pub fn prestart_all_core_threads(&self) -> Result<usize, SubmissionError> {
        self.inner.prestart_all_core_threads()
    }

    /// Updates the core pool size.
    ///
    /// Increasing the core size changes future admission and prestart limits,
    /// but it does not eagerly create workers or reschedule already queued
    /// work. Call [`Self::prestart_all_core_threads`] when eager creation
    /// is desired. Decreasing the core size lets excess idle workers retire
    /// according to the keep-alive policy. With `async-wait`, every successful
    /// update also sends an advisory capacity notification; it reserves no
    /// submission slot, so retry and handle saturation.
    ///
    /// # Parameters
    ///
    /// * `core_pool_size` - New core pool size.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the size is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorServiceBuilderError::CorePoolSizeExceedsMaximum`] when
    /// the new core size would exceed the current maximum size.
    #[inline]
    pub fn set_core_pool_size(&self, core_pool_size: usize) -> Result<(), ExecutorServiceBuilderError> {
        self.inner.set_core_pool_size(core_pool_size)
    }

    /// Updates the maximum pool size.
    ///
    /// Excess workers are not interrupted. They retire after finishing current
    /// work or timing out while idle. With `async-wait`, every successful
    /// update also sends an advisory capacity notification; increasing this
    /// limit can make a saturated submission admissible, but callers must
    /// retry and handle saturation because no slot is reserved.
    ///
    /// # Parameters
    ///
    /// * `maximum_pool_size` - New maximum pool size.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the size is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorServiceBuilderError::ZeroMaximumPoolSize`] when the
    /// maximum size is zero, or
    /// [`ExecutorServiceBuilderError::CorePoolSizeExceedsMaximum`]
    /// when it would be smaller than the current core size.
    #[inline]
    pub fn set_maximum_pool_size(&self, maximum_pool_size: usize) -> Result<(), ExecutorServiceBuilderError> {
        self.inner.set_maximum_pool_size(maximum_pool_size)
    }

    /// Updates how long excess idle workers may wait before exiting.
    ///
    /// # Parameters
    ///
    /// * `keep_alive` - New idle timeout for workers above the core size.
    ///
    /// Long timeouts are waited in bounded timer slices. If a timer wait fails,
    /// the worker waits for a monitor notification instead; idle retirement may
    /// be delayed until the next notification.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the timeout is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorServiceBuilderError::ZeroKeepAlive`] when `keep_alive`
    /// is zero.
    #[inline]
    pub fn set_keep_alive(&self, keep_alive: Duration) -> Result<(), ExecutorServiceBuilderError> {
        self.inner.set_keep_alive(keep_alive)
    }

    /// Updates whether core workers may also retire after keep-alive timeout.
    ///
    /// # Parameters
    ///
    /// * `allow` - Whether core workers are subject to idle timeout.
    #[inline]
    pub fn allow_core_thread_timeout(&self, allow: bool) {
        self.inner.allow_core_thread_timeout(allow);
    }
}

impl Drop for ThreadPool {
    /// Requests graceful shutdown when the pool value is dropped.
    fn drop(&mut self) {
        self.inner.shutdown();
    }
}

impl ExecutorService for ThreadPool {
    /// Handle returned when a callable task is submitted.
    type ResultHandle<R, E>
        = TaskHandle<R, E>
    where
        R: Send + 'static,
        E: Send + 'static;

    /// Handle returned when a tracked callable task is submitted.
    type TrackedHandle<R, E>
        = TrackedTask<R, E>
    where
        R: Send + 'static,
        E: Send + 'static;

    /// Accepts a runnable and queues it for pool workers.
    ///
    /// # Type Parameters
    ///
    /// * `T` - Runnable task type.
    /// * `E` - Error type produced by the task.
    ///
    /// # Parameters
    ///
    /// * `task` - Runnable task to execute on a pool worker.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] after shutdown, returns
    /// [`SubmissionError::Saturated`] when the bounded pool cannot accept
    /// more work, or returns [`SubmissionError::WorkerSpawnFailed`] when a
    /// required worker cannot be created.
    fn submit<T, E>(&self, task: T) -> Result<(), SubmissionError>
    where
        T: Runnable<E> + Send + 'static,
        E: Send + 'static,
    {
        self.inner
            .submit(PoolJob::detached(task))
            .map_err(PoolJobSubmissionError::into_submission_error)
    }

    /// Accepts a callable and queues it for pool workers.
    ///
    /// # Type Parameters
    ///
    /// * `C` - Callable task type.
    /// * `R` - Successful result type produced by the task.
    /// * `E` - Error type produced by the task.
    ///
    /// # Parameters
    ///
    /// * `task` - Callable to execute on a pool worker.
    ///
    /// # Returns
    ///
    /// A [`TaskHandle`] for the accepted task.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] after shutdown, returns
    /// [`SubmissionError::Saturated`] when the bounded pool cannot accept
    /// more work, or returns [`SubmissionError::WorkerSpawnFailed`] when a
    /// required worker cannot be created.
    fn submit_callable<C, R, E>(&self, task: C) -> Result<Self::ResultHandle<R, E>, SubmissionError>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let (handle, completion) = TaskEndpointPair::new().into_parts();
        let job = PoolJob::from_task(task, completion);
        self.inner
            .submit(job)
            .map_err(PoolJobSubmissionError::into_submission_error)?;
        Ok(handle)
    }

    /// Accepts a callable and queues it with a tracked handle.
    ///
    /// # Type Parameters
    ///
    /// * `C` - Callable task type.
    /// * `R` - Successful result type produced by the task.
    /// * `E` - Error type produced by the task.
    ///
    /// # Parameters
    ///
    /// * `task` - Callable task to execute on a pool worker.
    ///
    /// # Returns
    ///
    /// A tracked handle that observes the task's result and completion state.
    ///
    /// # Errors
    ///
    /// Returns [`SubmissionError::Shutdown`] after shutdown, returns
    /// [`SubmissionError::Saturated`] when the bounded pool cannot accept
    /// more work, or returns [`SubmissionError::WorkerSpawnFailed`] when a
    /// required worker cannot be created.
    fn submit_tracked_callable<C, R, E>(&self, task: C) -> Result<Self::TrackedHandle<R, E>, SubmissionError>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let (handle, completion) = TaskEndpointPair::new().into_tracked_parts();
        let job = PoolJob::from_task(task, completion);
        self.inner
            .submit(job)
            .map_err(PoolJobSubmissionError::into_submission_error)?;
        Ok(handle)
    }

    /// Stops accepting new tasks after already queued work is drained.
    ///
    /// Queued and running tasks remain eligible to complete normally.
    #[inline]
    fn shutdown(&self) {
        self.inner.shutdown();
    }

    /// Stops accepting tasks and cancels queued tasks that no worker has
    /// claimed. A claimed task is not revoked, even if its user callback has
    /// not begun.
    ///
    /// # Returns
    ///
    /// A report whose queued and cancelled counts include only jobs removed
    /// and cancelled by this stop call. A job removed first by a ticket
    /// cancellation is not included. The running count is sampled after
    /// in-flight submissions finish.
    #[inline]
    fn stop(&self) -> StopReport {
        self.inner.stop()
    }

    /// Returns the current lifecycle state.
    ///
    /// # Returns
    ///
    /// The pool's current lifecycle state.
    #[inline]
    fn lifecycle(&self) -> ExecutorServiceLifecycle {
        self.inner.lifecycle()
    }

    /// Returns whether shutdown has been requested.
    ///
    /// # Returns
    ///
    /// `true` after shutdown or stop has been requested.
    #[inline]
    fn is_not_running(&self) -> bool {
        self.inner.is_not_running()
    }

    /// Returns whether shutdown was requested and all workers have exited.
    ///
    /// # Returns
    ///
    /// `true` after all worker threads have terminated.
    #[inline]
    fn is_terminated(&self) -> bool {
        self.inner.is_terminated()
    }

    /// Blocks until the pool has terminated.
    fn wait_termination(&self) {
        self.inner.wait_for_termination();
    }

    /// Waits until this pool terminates or the timeout expires.
    ///
    /// # Parameters
    ///
    /// * `timeout` - Maximum duration to wait for termination.
    ///
    /// # Returns
    ///
    /// `true` if the pool terminated before the timeout, or `false` if the
    /// timeout elapsed first.
    fn wait_termination_timeout(&self, timeout: Duration) -> bool {
        self.inner.wait_for_termination_timeout(timeout)
    }
}

impl RecoverableExecutorService for ThreadPool {
    /// Attempts to submit a callable and returns it unchanged if admission
    /// fails.
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
    fn try_submit_callable<C, R, E>(&self, task: C) -> Result<Self::ResultHandle<R, E>, RejectedTask<C>>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        ThreadPool::try_submit_callable(self, task)
    }
}
