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

/// Callable task paired with its runner-side completion endpoint.
pub(crate) struct CompletablePoolTask<C, R, E> {
    /// Callable task to execute once a worker starts.
    pub(crate) task: C,
    /// Completion endpoint used to publish the task result.
    pub(crate) completion: TaskSlot<R, E>,
}

impl<C, R, E> PoolTask for CompletablePoolTask<C, R, E>
where
    C: Callable<R, E> + Send + 'static,
    R: Send + 'static,
    E: Send + 'static,
{
    fn accept(&self) -> Result<(), ()> {
        self.completion.accept();
        Ok(())
    }

    fn run(self: Box<Self>) {
        let Self { task, completion } = *self;
        TaskRunner::new(task).run(completion);
    }

    fn cancel(self: Box<Self>) {
        let Self { completion, .. } = *self;
        let _cancelled = completion.cancel_unstarted();
    }
}
