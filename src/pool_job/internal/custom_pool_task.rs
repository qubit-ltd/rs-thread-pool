// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Mutex;

use super::pool_task::PoolTask;
use crate::internal::contain_panic;

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
    /// Runs the acceptance callback while containing callback panics.
    fn accept(&self) -> Result<(), ()> {
        let accept = self
            .accept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(accept) = accept {
            return contain_panic(accept);
        }
        Ok(())
    }

    /// Runs the user callback while containing callback panics.
    fn run(self: Box<Self>) {
        let _ignored = contain_panic(|| {
            let Self { run, .. } = *self;
            let _ignored = contain_panic(run);
        });
    }

    /// Runs the cancellation callback while containing callback panics.
    fn cancel(self: Box<Self>) {
        let _ignored = contain_panic(|| {
            let Self { cancel, .. } = *self;
            let _ignored = contain_panic(cancel);
        });
    }
}
