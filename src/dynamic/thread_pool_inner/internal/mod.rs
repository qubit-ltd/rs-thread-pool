// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Internal worker reservation and admission types for
//! [`super::ThreadPoolInner`].

mod reserved_worker;
mod thread_pool_submit_guard;

pub(crate) use reserved_worker::ReservedWorker;
pub(crate) use thread_pool_submit_guard::ThreadPoolSubmitGuard;
