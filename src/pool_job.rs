// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Arc;
use std::sync::Mutex;

use qubit_executor::task::spi::TaskSlot;
use qubit_function::Callable;
use qubit_function::Runnable;

use crate::dynamic::thread_pool_inner::ThreadPoolInner;
use crate::internal::contain_panic;
pub use crate::pool_job_ticket::PoolJobTicket;

mod internal;
use internal::CompletablePoolTask;
use internal::CustomPoolTask;
use internal::PoolJobInner;
pub(crate) use internal::QueuedJob;
pub(crate) use internal::QueuedJobState;

/// Type-erased pool job with separate detached and cancellable forms.
///
/// # Examples
///
/// ```
/// use qubit_thread_pool::PoolJob;
/// use qubit_thread_pool::ThreadPool;
///
/// let pool = ThreadPool::new(1).expect("pool builds");
/// let job = PoolJob::new(Box::new(|| {}), Box::new(|| {}));
/// pool.submit_job(job).expect("job is accepted");
/// ```
#[must_use = "a pool job must be submitted or explicitly discarded"]
pub struct PoolJob {
    /// Internal job representation hidden behind method-only access.
    inner: PoolJobInner,
    /// Optional identity used only by the originating dynamic pool.
    queued: Option<Arc<QueuedJob>>,
}

impl PoolJob {
    /// Creates a custom cancellable job with no acceptance callback.
    ///
    /// Higher-level services that maintain their own task state usually want
    /// [`Self::with_accept`] instead, so they can publish acceptance only after
    /// the backing pool has accepted the job.
    /// Custom callbacks run synchronously and should not block. Panics raised
    /// by `run` or `cancel` are caught and ignored by the pool job wrapper. If
    /// a panic payload's destructor also panics, that nested payload is leaked
    /// to prevent a second unwind; aborting panic strategies and callbacks or
    /// destructors that block forever cannot be contained.
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
    pub fn new(run: Box<dyn FnOnce() + Send + 'static>, cancel: Box<dyn FnOnce() + Send + 'static>) -> Self {
        Self::with_accept(Box::new(|| {}), run, cancel)
    }

    /// Creates a custom cancellable job with an acceptance callback.
    ///
    /// The pool invokes `accept` exactly once after the submission crosses the
    /// acceptance boundary. If submission is rejected before acceptance,
    /// neither `accept`, `run`, nor `cancel` is invoked.
    ///
    /// Acceptance runs synchronously on the submitting thread after admission
    /// and any required worker creation succeeds. The callback may call
    /// `shutdown` on the same pool because shutdown closes admission
    /// without waiting. It must not call `stop`, `join`, or
    /// `wait_termination`, which may wait for this submission
    /// to leave admission. Keep callbacks short; non-blocking observation such
    /// as `stats` is safe. An acceptance panic is contained and reported as
    /// [`AcceptancePanicked`](crate::PoolJobSubmissionError::AcceptancePanicked);
    /// the job is neither run nor cancelled. Run and cancellation callback
    /// panics are caught and ignored. If a panic payload's destructor also
    /// panics, that nested payload is leaked to prevent a second unwind;
    /// aborting panic strategies and callbacks or destructors that block
    /// forever cannot be contained.
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
            queued: None,
            inner: PoolJobInner::Completable(Box::new(CustomPoolTask {
                accept: Mutex::new(Some(accept)),
                run,
                cancel,
            })),
        }
    }

    /// Creates a pool job from a typed callable task and completion endpoint.
    ///
    /// # Type Parameters
    ///
    /// * `C` - Callable task type.
    /// * `R` - Task result type.
    /// * `E` - Task error type.
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
            queued: None,
            inner: PoolJobInner::Completable(Box::new(CompletablePoolTask { task, completion })),
        }
    }

    /// Creates a pool job from a runnable task without retaining a result
    /// handle.
    ///
    /// # Type Parameters
    ///
    /// * `T` - Runnable task type.
    /// * `E` - Task error type.
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
            queued: None,
            inner: PoolJobInner::Detached {
                run: Box::new(move || {
                    let mut task = task;
                    let _ignored = contain_panic(|| task.run());
                }),
            },
        }
    }

    /// Associates custom callbacks with a ticket for `pool` without accepting
    /// them.
    ///
    /// # Returns
    ///
    /// The prepared job and its cancellation ticket. The job must be submitted
    /// to the originating pool for the ticket to cancel it.
    ///
    /// # Parameters
    ///
    /// * `pool` - Originating pool for the job.
    /// * `accept` - Callback invoked when the pool accepts the job.
    /// * `run` - Callback invoked when a worker starts the job.
    /// * `cancel` - Callback invoked when the queued job is cancelled.
    #[must_use = "the prepared job and its cancellation ticket must be handled"]
    pub(crate) fn prepare_cancellable(
        pool: &Arc<ThreadPoolInner>,
        accept: Box<dyn FnOnce() + Send + 'static>,
        run: Box<dyn FnOnce() + Send + 'static>,
        cancel: Box<dyn FnOnce() + Send + 'static>,
    ) -> (Self, PoolJobTicket) {
        let entry = Arc::new(QueuedJob::prepared(pool));
        let mut job = Self::with_accept(accept, run, cancel);
        job.queued = Some(Arc::clone(&entry));
        (
            job,
            PoolJobTicket {
                entry,
                pool: Arc::downgrade(pool),
            },
        )
    }

    /// Removes the optional ticket identity before moving this job into its
    /// entry.
    ///
    /// # Returns
    ///
    /// The associated queue entry, or `None` when this job has no ticket.
    pub(crate) fn take_queue_entry(&mut self) -> Option<Arc<QueuedJob>> {
        self.queued.take()
    }

    /// Asserts that a ticketed job is submitted only to its originating pool.
    ///
    /// # Parameters
    ///
    /// * `pool` - Pool receiving the job.
    pub(crate) fn assert_pool(&self, pool: &Arc<ThreadPoolInner>) {
        if let Some(entry) = &self.queued {
            assert!(
                entry.belongs_to_pool(pool),
                "ticketed job must be submitted to its originating pool"
            );
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

    /// Drops a rejected job without running callbacks, containing capture
    /// destructor panics so rejection keeps its original error result.
    pub(crate) fn discard(self) {
        let _ignored = contain_panic(|| drop(self));
    }

    /// Cancels this queued job if it has not been run first.
    ///
    /// Consumes the job and invokes the cancellation callback at most once.
    /// Contains panics from both cancellation and destruction of unused run
    /// captures so callers can always finish accounting and wake ticket
    /// waiters.
    pub(crate) fn cancel(self) {
        let _ignored = contain_panic(|| {
            if let PoolJobInner::Completable(task) = self.inner {
                task.cancel();
            }
        });
    }
}
