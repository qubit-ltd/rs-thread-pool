// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::panic::AssertUnwindSafe;
use std::panic::catch_unwind;
use std::sync::Mutex;

use qubit_executor::task::spi::TaskSlot;
use qubit_function::Callable;
use qubit_function::Runnable;

mod internal;
use internal::CompletablePoolTask;
use internal::CustomPoolTask;
use internal::PoolJobInner;

/// Type-erased pool job with separate detached and cancellable forms.
///
/// # Examples
///
/// ```
/// use qubit_thread_pool::PoolJob;
///
/// let _job = PoolJob::new(Box::new(|| {}), Box::new(|| {}));
/// ```
pub struct PoolJob {
    /// Internal job representation hidden behind method-only access.
    inner: PoolJobInner,
}

impl PoolJob {
    /// Creates a custom cancellable job with no acceptance callback.
    ///
    /// Higher-level services that maintain their own task state usually want
    /// [`Self::with_accept`] instead, so they can publish acceptance only after
    /// the backing pool has accepted the job.
    /// Custom callbacks run synchronously and should not block. Panics raised
    /// by `run` or `cancel` are caught and ignored by the pool job wrapper.
    ///
    /// # Parameters
    ///
    /// * `run` - Callback executed when a worker starts this job.
    /// * `cancel` - Callback executed if the accepted job is cancelled before
    ///   it starts.
    ///
    /// # Returns
    ///
    /// A custom type-erased job accepted by thread pools.
    pub fn new(
        run: Box<dyn FnOnce() + Send + 'static>,
        cancel: Box<dyn FnOnce() + Send + 'static>,
    ) -> Self {
        Self::with_accept(Box::new(|| {}), run, cancel)
    }

    /// Creates a custom cancellable job with an acceptance callback.
    ///
    /// The pool invokes `accept` exactly once after the submission crosses the
    /// acceptance boundary. If submission is rejected before acceptance,
    /// neither `accept`, `run`, nor `cancel` is invoked. Custom callbacks run
    /// synchronously and should not block. The acceptance callback must not
    /// synchronously call `shutdown`, `stop`, `join`, or `wait_termination` on
    /// the same pool, because those operations may wait for the in-flight
    /// submission and deadlock. Non-blocking observation such as `stats` is
    /// safe. Panics raised by these callbacks are caught and ignored by the
    /// pool job wrapper; an
    /// `accept` panic is reported to the pool as a failed acceptance
    /// callback.
    ///
    /// # Parameters
    ///
    /// * `accept` - Callback invoked once the pool accepts the job.
    /// * `run` - Callback executed when a worker starts this job.
    /// * `cancel` - Callback executed if the accepted job is cancelled before
    ///   it starts.
    ///
    /// # Returns
    ///
    /// A custom type-erased job accepted by thread pools.
    pub fn with_accept(
        accept: Box<dyn FnOnce() + Send + 'static>,
        run: Box<dyn FnOnce() + Send + 'static>,
        cancel: Box<dyn FnOnce() + Send + 'static>,
    ) -> Self {
        Self {
            inner: PoolJobInner::Completable(Box::new(CustomPoolTask {
                accept: Mutex::new(Some(accept)),
                run,
                cancel,
            })),
        }
    }

    /// Creates a pool job from a typed callable task and completion endpoint.
    ///
    /// # Parameters
    ///
    /// * `task` - Callable task to execute when a worker starts this job.
    /// * `completion` - Completion endpoint used to publish the typed result or
    ///   cancellation.
    ///
    /// # Returns
    ///
    /// A type-erased job that runs the task on worker start and cancels the
    /// completion endpoint if the job is cancelled while queued.
    pub(crate) fn from_task<C, R, E>(task: C, completion: TaskSlot<R, E>) -> Self
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        Self {
            inner: PoolJobInner::Completable(Box::new(CompletablePoolTask { task, completion })),
        }
    }

    /// Creates a pool job from a runnable task without retaining a result
    /// handle.
    ///
    /// # Parameters
    ///
    /// * `task` - Runnable task to execute when a worker starts this job.
    ///
    /// # Returns
    ///
    /// A type-erased job that runs the task and discards its final result. If
    /// the job is abandoned while queued, cancellation has no result endpoint
    /// to notify.
    pub(crate) fn detached<T, E>(task: T) -> Self
    where
        T: Runnable<E> + Send + 'static,
        E: Send + 'static,
    {
        Self {
            inner: PoolJobInner::Detached {
                run: Box::new(move || {
                    let mut task = task;
                    let _ignored = catch_unwind(AssertUnwindSafe(|| task.run()));
                }),
            },
        }
    }

    /// Marks this job as accepted by an executor service.
    ///
    /// Detached jobs do not have a completion endpoint, so this is a no-op for
    /// fire-and-forget submissions.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the acceptance callback completed, or `Err(())` when a
    /// custom acceptance callback panicked and was contained.
    pub(crate) fn accept(&self) -> Result<(), ()> {
        if let PoolJobInner::Completable(task) = &self.inner {
            return task.accept();
        }
        Ok(())
    }

    /// Runs this job if it has not been cancelled first.
    ///
    /// Consumes the job and invokes the run callback at most once.
    pub(crate) fn run(self) {
        match self.inner {
            PoolJobInner::Detached { run } => run(),
            PoolJobInner::Completable(task) => task.run(),
        }
    }

    /// Cancels this queued job if it has not been run first.
    ///
    /// Consumes the job and invokes the cancellation callback at most once.
    pub(crate) fn cancel(self) {
        if let PoolJobInner::Completable(task) = self.inner {
            task.cancel();
        }
    }
}
