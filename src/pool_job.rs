/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::panic::{
    AssertUnwindSafe,
    catch_unwind,
};

use qubit_executor::task::spi::{
    TaskRunner,
    TaskSlot,
};
use qubit_function::{
    Callable,
    Runnable,
};

/// Type-erased callable owned by a pool queue.
trait PoolTask: Send + 'static {
    /// Marks this task as accepted by an executor service.
    fn accept(&self);

    /// Runs this task and publishes its result if it was not cancelled first.
    fn run(self: Box<Self>);

    /// Cancels this task before it starts.
    fn cancel(self: Box<Self>);
}

/// Callable task paired with its runner-side completion endpoint.
struct CompletablePoolTask<C, R, E> {
    /// Callable task to execute once a worker starts this job.
    task: C,
    /// Completion endpoint used to publish the task result.
    completion: TaskSlot<R, E>,
}

impl<C, R, E> PoolTask for CompletablePoolTask<C, R, E>
where
    C: Callable<R, E> + Send + 'static,
    R: Send + 'static,
    E: Send + 'static,
{
    /// Marks this task as accepted by an executor service.
    fn accept(&self) {
        self.completion.accept();
    }

    /// Runs this task and publishes its result if it was not cancelled first.
    fn run(self: Box<Self>) {
        let Self { task, completion } = *self;
        TaskRunner::new(task).run(completion);
    }

    /// Publishes cancellation for an unstarted accepted task.
    fn cancel(self: Box<Self>) {
        let Self { completion, .. } = *self;
        let _cancelled = completion.cancel_unstarted();
    }
}

/// Type-erased pool job with separate detached and cancellable forms.
pub(crate) struct PoolJob {
    /// Internal job representation hidden behind method-only access.
    inner: PoolJobInner,
}

/// Private type-erased pool job representation.
enum PoolJobInner {
    /// Fire-and-forget job submitted without a completion endpoint.
    Detached {
        /// Callback executed once a worker starts the job.
        run: Box<dyn FnOnce() + Send + 'static>,
    },
    /// Job whose queued cancellation must complete a result endpoint.
    Completable(Box<dyn PoolTask>),
}

impl PoolJob {
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

    /// Creates a pool job from a runnable task without retaining a result handle.
    ///
    /// # Parameters
    ///
    /// * `task` - Runnable task to execute when a worker starts this job.
    ///
    /// # Returns
    ///
    /// A type-erased job that runs the task and discards its final result. If
    /// the job is abandoned while queued, cancellation has no result endpoint to
    /// notify.
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
    pub(crate) fn accept(&self) {
        if let PoolJobInner::Completable(task) = &self.inner {
            task.accept();
        }
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
