/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Tests for [`qubit_thread_pool::service::thread_pool`] — layout mirrors
//! `src/task/service/thread_pool/`.

mod fixed_thread_pool_builder_tests;
mod fixed_thread_pool_tests;
mod pool_job_tests;
mod support_tests;
mod thread_pool_build_error_tests;
mod thread_pool_builder_tests;
mod thread_pool_stats_tests;
mod thread_pool_tests;

pub(crate) use support_tests::{
    create_runtime,
    create_single_worker_pool,
    wait_started,
    wait_until,
};
