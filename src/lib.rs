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

pub mod delayed;
pub mod dynamic;
pub mod fixed;
mod pool_job;
mod thread_pool_stats;

pub use delayed::{
    DelayedTaskHandle,
    DelayedTaskScheduler,
};
pub use dynamic::{
    ThreadPool,
    ThreadPoolBuilder,
};
pub use fixed::{
    FixedThreadPool,
    FixedThreadPoolBuilder,
};
pub(crate) use pool_job::PoolJob;
pub use qubit_executor::service::{
    ExecutorBuildError,
    ExecutorService,
    ExecutorServiceLifecycle,
    RejectedExecution,
    StopReport,
};
pub use qubit_executor::{
    CancelResult,
    TaskExecutionError,
    TaskHandle,
    TaskResult,
    TaskResultHandle,
    TaskStatus,
    TrackedTask,
    TrackedTaskHandle,
    TryGet,
};
pub use thread_pool_stats::ThreadPoolStats;
