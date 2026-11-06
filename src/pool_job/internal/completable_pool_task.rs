// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use qubit_executor::task::spi::TaskRunner;
use qubit_executor::task::spi::TaskSlot;
use qubit_function::Callable;

use super::pool_task::PoolTask;

/// Couples a callable task with the endpoint that publishes its completion.
///
/// The task runner uses the endpoint to publish the task's result, while queued
/// cancellation uses it to complete the task without running the callable.
///
/// # Type Parameters
///
/// - `C`: Callable task submitted to the pool.
/// - `R`: Result value produced by the callable.
/// - `E`: Error value produced by the callable.
pub(crate) struct CompletablePoolTask<C, R, E> {
    /// Callable task executed when a worker starts this job.
    pub(crate) task: C,
    /// Endpoint used to publish the result or complete cancellation before
    /// execution.
    pub(crate) completion: TaskSlot<R, E>,
}

impl<C, R, E> PoolTask for CompletablePoolTask<C, R, E>
where
    C: Callable<R, E> + Send + 'static,
    R: Send + 'static,
    E: Send + 'static,
{
    /// Publishes acceptance to the task completion endpoint.
    fn accept(&self) -> Result<(), ()> {
        self.completion.accept();
        Ok(())
    }

    /// Runs the callable and publishes its result.
    fn run(self: Box<Self>) {
        let Self { task, completion } = *self;
        TaskRunner::new(task).run(completion);
    }

    /// Completes the endpoint without running the callable.
    fn cancel(self: Box<Self>) {
        let Self { completion, .. } = *self;
        let _cancelled = completion.cancel_unstarted();
    }
}
