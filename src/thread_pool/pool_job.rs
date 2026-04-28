/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
use qubit_executor::{
    TaskCompletion,
    TaskRunner,
};
use qubit_function::Callable;

/// Type-erased pool job with a cancellation path for queued work.
///
/// `PoolJob` is a low-level extension point for building custom services on
/// top of [`super::thread_pool::ThreadPool`]. The pool calls the run callback after a worker takes
/// the job, or the cancel callback if the job is still queued during immediate
/// shutdown.
pub struct PoolJob {
    /// Callback executed once a worker starts the job.
    run: Box<dyn FnOnce() + Send + 'static>,
    /// Callback executed if the job is cancelled before a worker starts it.
    cancel: Box<dyn FnOnce() + Send + 'static>,
}

impl PoolJob {
    /// Creates a pool job from run and cancel callbacks.
    ///
    /// # Parameters
    ///
    /// * `run` - Callback executed once a worker starts this job.
    /// * `cancel` - Callback executed if this job is cancelled while queued.
    ///
    /// # Returns
    ///
    /// A type-erased job accepted by [`super::thread_pool::ThreadPool::submit_job`].
    pub fn new(
        run: Box<dyn FnOnce() + Send + 'static>,
        cancel: Box<dyn FnOnce() + Send + 'static>,
    ) -> Self {
        Self { run, cancel }
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
    /// completion endpoint if the job is abandoned while queued.
    pub fn from_task<C, R, E>(task: C, completion: TaskCompletion<R, E>) -> Self
    where
        C: Callable<R, E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let cancel_completion = completion.clone();
        Self::new(
            Box::new(move || {
                TaskRunner::new(task).run(completion);
            }),
            Box::new(move || {
                cancel_completion.cancel();
            }),
        )
    }

    /// Runs this job if it has not been cancelled first.
    ///
    /// Consumes the job and invokes the run callback at most once.
    pub(crate) fn run(self) {
        (self.run)();
    }

    /// Cancels this queued job if it has not been run first.
    ///
    /// Consumes the job and invokes the cancellation callback at most once.
    pub(crate) fn cancel(self) {
        (self.cancel)();
    }
}
