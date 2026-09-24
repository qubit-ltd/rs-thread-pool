// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::panic::AssertUnwindSafe;
use std::panic::catch_unwind;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::Weak;
use std::thread::ThreadId;

use qubit_executor::task::spi::TaskSlot;
use qubit_function::Callable;
use qubit_function::Runnable;

use crate::dynamic::thread_pool_inner::ThreadPoolInner;
pub use crate::pool_job_ticket::PoolJobTicket;

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
    /// panics are caught and ignored.
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
                    let _ignored = catch_unwind(AssertUnwindSafe(|| task.run()));
                }),
            },
        }
    }

    /// Associates custom callbacks with a ticket for `pool` without accepting
    /// them.
    pub(crate) fn prepare_cancellable(
        pool: &Arc<ThreadPoolInner>,
        accept: Box<dyn FnOnce() + Send + 'static>,
        run: Box<dyn FnOnce() + Send + 'static>,
        cancel: Box<dyn FnOnce() + Send + 'static>,
    ) -> (Self, PoolJobTicket) {
        let entry = Arc::new(QueuedJob {
            state: Mutex::new(QueuedJobState::Prepared),
            changed: Condvar::new(),
            pool: Arc::downgrade(pool),
        });
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
    pub(crate) fn take_queue_entry(&mut self) -> Option<Arc<QueuedJob>> {
        self.queued.take()
    }

    /// Asserts that a ticketed job is submitted only to its originating pool.
    pub(crate) fn assert_pool(&self, pool: &Arc<ThreadPoolInner>) {
        if let Some(entry) = &self.queued {
            assert!(
                entry.pool.ptr_eq(&Arc::downgrade(pool)),
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
        let _ignored = catch_unwind(AssertUnwindSafe(|| drop(self)));
    }

    /// Cancels this queued job if it has not been run first.
    ///
    /// Consumes the job and invokes the cancellation callback at most once.
    /// Contains panics from both cancellation and destruction of unused run
    /// captures so callers can always finish accounting and wake ticket
    /// waiters.
    pub(crate) fn cancel(self) {
        let _ignored = catch_unwind(AssertUnwindSafe(|| {
            if let PoolJobInner::Completable(task) = self.inner {
                task.cancel();
            }
        }));
    }
}

/// Shared queue identity; all queue ownership transfers lock queue before
/// entry.
pub(crate) struct QueuedJob {
    /// Job ownership or the acceptance/cancellation transition currently in
    /// progress.
    pub(crate) state: Mutex<QueuedJobState>,
    /// Wakes the caller that reserved cancellation during acceptance.
    pub(crate) changed: Condvar,
    /// Originating pool for prepared jobs; ordinary entries use an empty weak
    /// pointer.
    pool: Weak<ThreadPoolInner>,
}

impl QueuedJob {
    /// Wraps an ordinary accepted job in an entry for the dynamic FIFO.
    pub(crate) fn new(job: PoolJob) -> Self {
        Self {
            state: Mutex::new(QueuedJobState::Queued(job)),
            changed: Condvar::new(),
            pool: Weak::new(),
        }
    }

    /// Locks the ownership state, recovering an already poisoned mutex.
    pub(crate) fn lock(&self) -> MutexGuard<'_, QueuedJobState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Takes the queued callbacks once; callers must already hold the queue
    /// lock.
    pub(crate) fn take_queued(&self) -> Option<PoolJob> {
        let mut state = self.lock();
        match std::mem::replace(&mut *state, QueuedJobState::Taken) {
            QueuedJobState::Queued(job) => Some(job),
            previous => {
                *state = previous;
                None
            }
        }
    }
}

/// States preceding and following the unique transfer of a job's callbacks.
pub(crate) enum QueuedJobState {
    /// Prepared but not yet admitted; ticket cancellation is not valid.
    Prepared,
    /// Acceptance runs outside all locks; one ticket may reserve cancellation.
    Accepting { owner: ThreadId, cancel_requested: bool },
    /// Accepted and visible in the FIFO; contains the sole callback owner.
    Queued(PoolJob),
    /// Worker, stop, or a ticket has taken the queued callbacks.
    Taken,
    /// The submitter is cancelling an accepted job before queue publication.
    Cancelling,
    /// Reserved cancellation and accounting have finished.
    Cancelled,
    /// Acceptance panicked, so no callbacks may run.
    Rejected,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;

    use qubit_executor::service::ExecutorService;

    use super::QueuedJobState;
    use crate::PoolJobSubmissionError;
    use crate::ThreadPool;

    /// Observes the private reservation boundary to make this acceptance race
    /// deterministic, including an idle worker that could otherwise claim
    /// first.
    #[test]
    fn test_ticket_reserved_during_accept_never_publishes_to_worker() {
        for acceptance_panics in [false, true] {
            let pool = Arc::new(ThreadPool::new(1).expect("pool builds"));
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let (cancelled_tx, cancelled_rx) = mpsc::channel();
            let (ran_tx, ran_rx) = mpsc::channel();
            let (job, ticket) = pool.prepare_cancellable_job(
                Box::new(move || {
                    started_tx.send(thread::current().id()).expect("accept starts");
                    release_rx.recv().expect("accept released");
                    assert!(!acceptance_panics, "acceptance fails");
                }),
                Box::new(move || ran_tx.send(()).expect("run observed")),
                Box::new(move || cancelled_tx.send(thread::current().id()).expect("cancel observed")),
            );
            let entry = Arc::clone(&ticket.entry);
            let (submitted_tx, submitted_rx) = mpsc::channel();
            let submit_pool = Arc::clone(&pool);
            thread::spawn(move || submitted_tx.send(submit_pool.submit_job(job)).expect("submit result"));
            let submitter = started_rx.recv_timeout(Duration::from_secs(2)).expect("accept starts");
            let (result_tx, result_rx) = mpsc::channel();
            thread::spawn(move || result_tx.send(ticket.cancel_queued()).expect("cancel result"));
            let deadline = Instant::now() + Duration::from_secs(2);
            while !matches!(
                *entry.lock(),
                QueuedJobState::Accepting {
                    cancel_requested: true,
                    ..
                }
            ) {
                assert!(
                    Instant::now() < deadline,
                    "ticket reserves cancellation before accept returns"
                );
                thread::yield_now();
            }
            assert_eq!(pool.stats().queued_tasks, 0);
            release_tx.send(()).expect("release acceptance");
            assert_eq!(
                result_rx.recv_timeout(Duration::from_secs(2)).expect("cancel finishes"),
                !acceptance_panics
            );
            let submission = submitted_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("submit finishes");
            if acceptance_panics {
                assert_eq!(submission, Err(PoolJobSubmissionError::AcceptancePanicked));
                assert!(cancelled_rx.try_recv().is_err());
            } else {
                submission.expect("job accepted");
                assert_eq!(cancelled_rx.try_recv().expect("cancelled on submitter"), submitter);
            }
            assert!(
                ran_rx.try_recv().is_err(),
                "reserved job must never be published to idle worker"
            );
            pool.shutdown();
            assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
            let stats = pool.stats();
            assert_eq!(stats.submitted_tasks, usize::from(!acceptance_panics));
            assert_eq!(stats.cancelled_tasks, stats.submitted_tasks);
            assert_eq!(stats.queued_tasks, 0);
            assert_eq!(stats.running_tasks, 0);
        }
    }

    /// Panics while releasing a queued run closure rather than while calling
    /// cancel.
    struct PanickingRunCapture;

    impl Drop for PanickingRunCapture {
        fn drop(&mut self) {
            panic!("reserved cancellation run capture destructor panicked");
        }
    }

    #[test]
    fn test_ticket_reserved_cancellation_contains_capture_drop_panic_and_wakes_waiter() {
        let pool = Arc::new(ThreadPool::new(1).expect("pool builds"));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        let capture = PanickingRunCapture;
        let (job, ticket) = pool.prepare_cancellable_job(
            Box::new(move || {
                started_tx.send(()).expect("accept starts");
                release_rx.recv().expect("accept released");
            }),
            Box::new(move || drop(capture)),
            Box::new(move || cancelled_tx.send(()).expect("cancel observed")),
        );
        let entry = Arc::clone(&ticket.entry);
        let (submitted_tx, submitted_rx) = mpsc::channel();
        let submit_pool = Arc::clone(&pool);
        thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| submit_pool.submit_job(job)));
            submitted_tx.send(result).expect("submit result");
        });
        started_rx.recv_timeout(Duration::from_secs(2)).expect("accept starts");
        let (result_tx, result_rx) = mpsc::channel();
        thread::spawn(move || result_tx.send(ticket.cancel_queued()).expect("cancel result"));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(
            *entry.lock(),
            QueuedJobState::Accepting {
                cancel_requested: true,
                ..
            }
        ) {
            assert!(
                Instant::now() < deadline,
                "ticket reserves cancellation before accept returns"
            );
            thread::yield_now();
        }
        release_tx.send(()).expect("release acceptance");
        let submission = submitted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("submit finishes");
        // Close admission even when the reproduction unwinds in the submitter.
        pool.shutdown();
        assert!(
            submission.is_ok(),
            "capture Drop panic must not escape reserved cancellation"
        );
        submission
            .expect("submitter does not unwind")
            .expect("job was accepted");
        assert!(
            result_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("reserved ticket must wake")
        );
        assert!(matches!(*entry.lock(), QueuedJobState::Cancelled));
        cancelled_rx.try_recv().expect("cancellation callback ran once");
        assert!(cancelled_rx.try_recv().is_err());
        assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
        let stats = pool.stats();
        assert_eq!(stats.submitted_tasks, 1);
        assert_eq!(stats.cancelled_tasks, 1);
        assert_eq!(stats.completed_tasks, 0);
        assert_eq!(stats.queued_tasks, 0);
        assert_eq!(stats.running_tasks, 0);
    }

    #[test]
    fn test_ticket_rejection_contains_capture_drop_panic() {
        for rejection in 0..3 {
            for run_capture_panics in [true, false] {
                let pool = ThreadPool::builder()
                    .pool_size(1)
                    .queue_capacity(1)
                    .prestart_core_threads()
                    .build()
                    .expect("pool builds");
                let (started_tx, started_rx) = mpsc::channel();
                let (release_tx, release_rx) = mpsc::channel();
                if rejection == 2 {
                    pool.submit_job(super::PoolJob::new(
                        Box::new(move || {
                            started_tx.send(()).expect("blocking worker starts");
                            release_rx.recv().expect("blocking worker released");
                        }),
                        Box::new(|| {}),
                    ))
                    .expect("blocking job accepted");
                    started_rx.recv_timeout(Duration::from_secs(2)).expect("worker starts");
                    pool.submit_job(super::PoolJob::new(Box::new(|| {}), Box::new(|| {})))
                        .expect("queue fills");
                }
                let capture = PanickingRunCapture;
                let panicking: Box<dyn FnOnce() + Send> = Box::new(move || drop(capture));
                let ignored: Box<dyn FnOnce() + Send> = Box::new(|| {});
                let (run, cancel) = if run_capture_panics {
                    (panicking, ignored)
                } else {
                    (ignored, panicking)
                };
                let (job, ticket) =
                    pool.prepare_cancellable_job(Box::new(|| panic!("acceptance callback panics")), run, cancel);
                if rejection == 1 {
                    pool.shutdown();
                }
                let submission = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pool.submit_job(job)));
                pool.shutdown();
                if rejection == 2 {
                    release_tx.send(()).expect("release blocking worker");
                }
                assert!(
                    submission.is_ok(),
                    "rejected captures must not replace the submission error with unwind"
                );
                let expected = match rejection {
                    0 => PoolJobSubmissionError::AcceptancePanicked,
                    1 => PoolJobSubmissionError::Rejected(qubit_executor::service::SubmissionError::Shutdown),
                    _ => PoolJobSubmissionError::Rejected(qubit_executor::service::SubmissionError::Saturated),
                };
                assert_eq!(submission.expect("submission does not unwind"), Err(expected));
                assert!(!ticket.cancel_queued());
                if rejection == 0 {
                    assert!(matches!(*ticket.entry.lock(), QueuedJobState::Rejected));
                } else {
                    assert!(matches!(*ticket.entry.lock(), QueuedJobState::Prepared));
                }
                assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
                let stats = pool.stats();
                assert_eq!(stats.submitted_tasks, if rejection == 2 { 2 } else { 0 });
                assert_eq!(stats.cancelled_tasks, 0);
                assert_eq!(stats.completed_tasks, stats.submitted_tasks);
                assert_eq!(stats.queued_tasks, 0);
                assert_eq!(stats.running_tasks, 0);
            }
        }
    }
}
