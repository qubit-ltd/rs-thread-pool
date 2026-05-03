/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! # Qubit Thread Pool
//!
//! Dynamic and fixed-size thread pool executor services.
//!

mod fixed_thread_pool;
mod fixed_thread_pool_builder;
mod queue_steal_source;
mod thread_pool;
mod worker_queue;
mod worker_runtime;

pub use fixed_thread_pool::FixedThreadPool;
pub use fixed_thread_pool_builder::FixedThreadPoolBuilder;
pub use qubit_executor::service::{
    ExecutorService,
    RejectedExecution,
    ShutdownReport,
};
pub use qubit_executor::{
    TaskExecutionError,
    TaskHandle,
    TaskResult,
};
pub use thread_pool::{
    PoolJob,
    ThreadPool,
    ThreadPoolBuildError,
    ThreadPoolBuilder,
    ThreadPoolStats,
};

/// Executor service compatibility exports for thread-pool users.
pub mod service {
    pub use crate::{
        ExecutorService,
        FixedThreadPool,
        FixedThreadPoolBuilder,
        PoolJob,
        RejectedExecution,
        ShutdownReport,
        ThreadPool,
        ThreadPoolBuildError,
        ThreadPoolBuilder,
        ThreadPoolStats,
    };
}
