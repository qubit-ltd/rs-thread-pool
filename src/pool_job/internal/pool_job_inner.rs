// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use super::pool_task::PoolTask;

/// Private type-erased pool job representation.
pub(crate) enum PoolJobInner {
    /// Fire-and-forget job executed once a worker starts it.
    Detached {
        /// Callback executed once a worker starts the job.
        run: Box<dyn FnOnce() + Send + 'static>,
    },
    /// Job whose queued cancellation must complete a result endpoint.
    Completable(Box<dyn PoolTask>),
}
