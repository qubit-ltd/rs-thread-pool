// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Internal type-erased task representations for [`super::PoolJob`].

mod completable_pool_task;
mod custom_pool_task;
mod pool_job_inner;
mod pool_task;

pub(super) use completable_pool_task::CompletablePoolTask;
pub(super) use custom_pool_task::CustomPoolTask;
pub(super) use pool_job_inner::PoolJobInner;
