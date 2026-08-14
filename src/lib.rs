// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! # Qubit Thread Pool
//!
//! Dynamic and fixed-size thread pool executor services.

pub mod dynamic;
pub mod fixed;
mod pool_job;
mod thread_pool_hooks;
mod thread_pool_stats;

pub use dynamic::ThreadPool;
pub use dynamic::ThreadPoolBuilder;
pub use fixed::FixedThreadPool;
pub use fixed::FixedThreadPoolBuilder;
pub use pool_job::PoolJob;
use qubit_executor::service::ExecutorServiceBuilderError;
pub use thread_pool_hooks::ThreadPoolHooks;
pub use thread_pool_stats::ThreadPoolStats;
