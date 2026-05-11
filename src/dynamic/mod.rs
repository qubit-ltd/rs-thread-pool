/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Dynamic thread pool implementation.

mod thread_pool;
mod thread_pool_builder;
pub(crate) mod thread_pool_config;
pub(crate) mod thread_pool_inner;
pub(crate) mod thread_pool_state;
pub(crate) mod thread_pool_worker;
pub(crate) mod thread_pool_worker_queue;
pub(crate) mod thread_pool_worker_runtime;

pub use thread_pool::ThreadPool;
pub use thread_pool_builder::ThreadPoolBuilder;
