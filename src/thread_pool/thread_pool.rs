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
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use qubit_function::Callable;

use qubit_executor::{
    TaskCompletionPair,
    TaskHandle,
    TaskRunner,
};

use super::pool_job::PoolJob;
use super::thread_pool_build_error::ThreadPoolBuildError;
use super::thread_pool_builder::ThreadPoolBuilder;
use super::thread_pool_inner::ThreadPoolInner;
use super::thread_pool_stats::ThreadPoolStats;
use qubit_executor::service::{
    ExecutorService,
    RejectedExecution,
    ShutdownReport,
};

/// OS thread pool implementing [`ExecutorService`].
///
/// `ThreadPool` accepts fallible tasks, stores them in an internal FIFO queue,
/// and executes them on worker threads. Workers are created lazily up to the
/// configured core size, queued after that, and may grow up to the maximum size
/// when a bounded queue is full. Submitted tasks return [`TaskHandle`], which
/// supports both blocking [`TaskHandle::get`] and async `.await` result
/// retrieval.
///
/// `shutdown` is graceful: already accepted queued tasks are allowed to run.
/// `shutdown_now` is abrupt: queued tasks that have not started are completed
/// with [`TaskExecutionError::Cancelled`](qubit_executor::TaskExecutionError::Cancelled).
///
pub struct ThreadPool {
    /// Shared pool state and worker coordination primitives.
    inner: Arc<ThreadPoolInner>,
}

impl ThreadPool {
    pub(super) fn from_inner(inner: Arc<ThreadPoolInner>) -> Self {
        Self { inner }
    }

    /// Creates a thread pool with equal core and maximum pool sizes.
    ///
    /// # Parameters
    ///
    /// * `pool_size` - Value applied as both core and maximum pool size.
    ///
    /// # Returns
    ///
    /// `Ok(ThreadPool)` if all workers are spawned successfully.
    ///
    /// # Errors
    ///
    /// Returns [`ThreadPoolBuildError`] if the resulting maximum pool size is
    /// zero or a worker thread cannot be spawned.
    #[inline]
    pub fn new(pool_size: usize) -> Result<Self, ThreadPoolBuildError> {
        Self::builder().pool_size(pool_size).build()
    }

    /// Creates a builder for configuring a thread pool.
    ///
    /// # Returns
    ///
    /// A builder with default core and maximum pool sizes and an unbounded queue.
    #[inline]
    pub fn builder() -> ThreadPoolBuilder {
        ThreadPoolBuilder::default()
    }

    /// Returns the number of queued tasks waiting for a worker.
    ///
    /// # Returns
    ///
    /// The number of accepted tasks that have not started yet.
    #[inline]
    pub fn queued_count(&self) -> usize {
        self.inner.read_state(|state| state.queued_tasks)
    }

    /// Returns the number of tasks currently held by workers.
    ///
    /// # Returns
    ///
    /// The number of tasks that workers have taken from the queue and have not
    /// yet finished processing.
    #[inline]
    pub fn running_count(&self) -> usize {
        self.inner.read_state(|state| state.running_tasks)
    }

    /// Returns how many worker threads are still running in this pool.
    ///
    /// # Returns
    ///
    /// The number of live worker loops still owned by this pool. This is a
    /// runtime count and is not required to match configured
    /// [`Self::core_pool_size`] or [`Self::maximum_pool_size`].
    #[inline]
    pub fn live_worker_count(&self) -> usize {
        self.inner.read_state(|state| state.live_workers)
    }

    /// Returns the configured core pool size.
    ///
    /// # Returns
    ///
    /// The number of workers kept for normal load before tasks are queued.
    #[inline]
    pub fn core_pool_size(&self) -> usize {
        self.inner.read_state(|state| state.core_pool_size)
    }

    /// Returns the configured maximum pool size.
    ///
    /// # Returns
    ///
    /// The maximum number of worker threads this pool may create.
    #[inline]
    pub fn maximum_pool_size(&self) -> usize {
        self.inner.read_state(|state| state.maximum_pool_size)
    }

    /// Returns a point-in-time snapshot of pool counters.
    ///
    /// # Returns
    ///
    /// A snapshot containing worker, queue, and task counters observed under
    /// the pool state lock.
    #[inline]
    pub fn stats(&self) -> ThreadPoolStats {
        self.inner.stats()
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
    /// Returns [`RejectedExecution::Shutdown`] if the pool is shut down, or
    /// [`RejectedExecution::WorkerSpawnFailed`] if worker creation fails.
    #[inline]
    pub fn prestart_core_thread(&self) -> Result<bool, RejectedExecution> {
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
    /// Returns [`RejectedExecution::Shutdown`] if the pool is shut down, or
    /// [`RejectedExecution::WorkerSpawnFailed`] if worker creation fails.
    #[inline]
    pub fn prestart_all_core_threads(&self) -> Result<usize, RejectedExecution> {
        self.inner.prestart_all_core_threads()
    }

    /// Updates the core pool size.
    ///
    /// Increasing the core size does not eagerly create new workers unless
    /// queued work is waiting. Call [`Self::prestart_all_core_threads`] when
    /// eager creation is desired. Decreasing the core size lets excess idle
    /// workers retire according to the keep-alive policy.
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
    /// Returns [`ThreadPoolBuildError::CorePoolSizeExceedsMaximum`] when the
    /// new core size would exceed the current maximum size.
    pub fn set_core_pool_size(&self, core_pool_size: usize) -> Result<(), ThreadPoolBuildError> {
        self.inner.set_core_pool_size(core_pool_size)
    }

    /// Updates the maximum pool size.
    ///
    /// Excess workers are not interrupted. They retire after finishing current
    /// work or timing out while idle.
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
    /// Returns [`ThreadPoolBuildError::ZeroMaximumPoolSize`] when the maximum
    /// size is zero, or [`ThreadPoolBuildError::CorePoolSizeExceedsMaximum`]
    /// when it would be smaller than the current core size.
    pub fn set_maximum_pool_size(
        &self,
        maximum_pool_size: usize,
    ) -> Result<(), ThreadPoolBuildError> {
        self.inner.set_maximum_pool_size(maximum_pool_size)
    }

    /// Updates how long excess idle workers may wait before exiting.
    ///
    /// # Parameters
    ///
    /// * `keep_alive` - New idle timeout for workers above the core size.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the timeout is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ThreadPoolBuildError::ZeroKeepAlive`] when `keep_alive` is
    /// zero.
    pub fn set_keep_alive(&self, keep_alive: Duration) -> Result<(), ThreadPoolBuildError> {
        self.inner.set_keep_alive(keep_alive)
    }

    /// Updates whether core workers may also retire after keep-alive timeout.
    ///
    /// # Parameters
    ///
    /// * `allow` - Whether core workers are subject to idle timeout.
    pub fn allow_core_thread_timeout(&self, allow: bool) {
        self.inner.allow_core_thread_timeout(allow);
    }

    /// Submits an already type-erased pool job.
    ///
    /// This low-level hook is intended for higher-level service crates that
    /// need to attach their own lifecycle callbacks while still using this
    /// pool's queueing, cancellation, and shutdown behavior.
    ///
    /// # Parameters
    ///
    /// * `job` - Type-erased job containing run and cancellation callbacks.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the job is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`RejectedExecution::Shutdown`] after shutdown, returns
    /// [`RejectedExecution::Saturated`] when a bounded pool cannot accept more
    /// work, or returns [`RejectedExecution::WorkerSpawnFailed`] when the pool
    /// fails to create a required worker.
    pub fn submit_job(&self, job: PoolJob) -> Result<(), RejectedExecution> {
        self.inner.submit(job)
    }
}

impl Drop for ThreadPool {
    /// Requests graceful shutdown when the pool value is dropped.
    fn drop(&mut self) {
        self.inner.shutdown();
    }
}

impl ExecutorService for ThreadPool {
    type Handle<R, E>
        = TaskHandle<R, E>
    where
        R: Send + 'static,
        E: Send + 'static;

    type Termination<'a>
        = Pin<Box<dyn Future<Output = ()> + Send + 'a>>
    where
        Self: 'a;

    /// Accepts a callable and queues it for pool workers.
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
    /// Returns [`RejectedExecution::Shutdown`] after shutdown, returns
    /// [`RejectedExecution::Saturated`] when the bounded pool cannot accept
    /// more work, or returns [`RejectedExecution::WorkerSpawnFailed`] when a
    /// required worker cannot be created.
    fn submit_callable<C, R, E>(&self, task: C) -> Result<Self::Handle<R, E>, RejectedExecution>
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let (handle, completion) = TaskCompletionPair::new().into_parts();
        let completion_for_run = completion.clone();
        let job = PoolJob::new(
            Box::new(move || {
                TaskRunner::new(task).run(completion_for_run);
            }),
            Box::new(move || {
                completion.cancel();
            }),
        );
        self.inner.submit(job)?;
        Ok(handle)
    }

    /// Stops accepting new tasks after already queued work is drained.
    ///
    /// Queued and running tasks remain eligible to complete normally.
    #[inline]
    fn shutdown(&self) {
        self.inner.shutdown();
    }

    /// Stops accepting tasks and cancels queued tasks that have not started.
    ///
    /// # Returns
    ///
    /// A report containing the number of queued jobs cancelled and the number
    /// of jobs running at the time of the request.
    #[inline]
    fn shutdown_now(&self) -> ShutdownReport {
        self.inner.shutdown_now()
    }

    /// Returns whether shutdown has been requested.
    #[inline]
    fn is_shutdown(&self) -> bool {
        self.inner.is_shutdown()
    }

    /// Returns whether shutdown was requested and all workers have exited.
    #[inline]
    fn is_terminated(&self) -> bool {
        self.inner.is_terminated()
    }

    /// Waits until the pool has terminated.
    ///
    /// This future blocks the polling thread while waiting on a condition
    /// variable.
    ///
    /// # Returns
    ///
    /// A future that resolves when shutdown has been requested, the queue is
    /// empty, no task is running, and all worker loops have exited.
    fn await_termination(&self) -> Self::Termination<'_> {
        Box::pin(async move {
            self.inner.wait_for_termination();
        })
    }
}
