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
};

use qubit_executor::service::{
    ExecutorService,
    RejectedExecution,
    ShutdownReport,
};
use qubit_executor::{
    TaskCompletionPair,
    TaskHandle,
};
use qubit_function::Callable;

use super::fixed_thread_pool_builder::FixedThreadPoolBuilder;
use super::fixed_thread_pool_inner::FixedThreadPoolInner;
use super::fixed_worker::FixedWorker;
use super::fixed_worker_runtime::FixedWorkerRuntime;
use crate::{
    PoolJob,
    ThreadPoolBuildError,
    ThreadPoolStats,
};

/// Fixed-size thread pool implementing [`ExecutorService`].
///
/// `FixedThreadPool` prestarts a fixed number of worker threads and does not
/// support runtime pool-size changes. Use [`crate::ThreadPool`] when dynamic
/// core/maximum sizes or keep-alive policies are required.
pub struct FixedThreadPool {
    /// Shared fixed pool state.
    inner: Arc<FixedThreadPoolInner>,
}

impl FixedThreadPool {
    /// Builds a fixed pool from a validated [`FixedThreadPoolBuilder`].
    ///
    /// # Parameters
    ///
    /// * `builder` - Configuration produced by [`FixedThreadPoolBuilder`].
    ///
    /// # Returns
    ///
    /// A fixed thread-pool handle with workers already started.
    ///
    /// # Errors
    ///
    /// Returns [`ThreadPoolBuildError`] when a worker thread cannot be spawned.
    pub(crate) fn new_with_builder(
        builder: FixedThreadPoolBuilder,
    ) -> Result<Self, ThreadPoolBuildError> {
        let FixedThreadPoolBuilder {
            pool_size,
            queue_capacity,
            thread_name_prefix,
            stack_size,
        } = builder;
        let mut worker_runtimes = Vec::with_capacity(pool_size);
        let mut worker_queues = Vec::with_capacity(pool_size);
        for index in 0..pool_size {
            let worker_runtime = FixedWorkerRuntime::new(index);
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
            let thread_name = format!("{}-{}", thread_name_prefix, index);
            let mut builder = std::thread::Builder::new().name(thread_name);
            if let Some(stack_size) = stack_size {
                builder = builder.stack_size(stack_size);
            }
            if let Err(source) =
                builder.spawn(move || FixedWorker::run(worker_inner, worker_runtime))
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

impl Default for FixedThreadPool {
    /// Creates a fixed thread pool using [`FixedThreadPoolBuilder::default`].
    ///
    /// # Returns
    ///
    /// A fixed thread pool with CPU parallelism defaults and prestarted workers.
    ///
    /// # Panics
    ///
    /// Panics when the default builder fails to spawn a worker thread.
    fn default() -> Self {
        FixedThreadPoolBuilder::default()
            .build()
            .expect("failed to build default FixedThreadPool")
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
