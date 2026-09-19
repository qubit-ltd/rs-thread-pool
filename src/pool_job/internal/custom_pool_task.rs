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

use super::pool_task::PoolTask;

/// Custom job callbacks supplied by higher-level services.
pub(crate) struct CustomPoolTask {
    /// Callback invoked once the pool accepts the job.
    pub(crate) accept: Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>,
    /// Callback executed once a worker starts this job.
    pub(crate) run: Box<dyn FnOnce() + Send + 'static>,
    /// Callback executed if the accepted job is cancelled before it starts.
    pub(crate) cancel: Box<dyn FnOnce() + Send + 'static>,
}

impl PoolTask for CustomPoolTask {
    fn accept(&self) -> Result<(), ()> {
        if let Some(accept) = self
            .accept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            return catch_unwind(AssertUnwindSafe(accept)).map_err(|_| ());
        }
        Ok(())
    }

    fn run(self: Box<Self>) {
        let Self { run, .. } = *self;
        let _ignored = catch_unwind(AssertUnwindSafe(run));
    }

    fn cancel(self: Box<Self>) {
        let Self { cancel, .. } = *self;
        let _ignored = catch_unwind(AssertUnwindSafe(cancel));
    }
}
