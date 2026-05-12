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
mod thread_pool_hooks;
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
pub use pool_job::PoolJob;
pub use qubit_executor::service::{
    ExecutorService,
    ExecutorServiceBuilderError,
    ExecutorServiceLifecycle,
    StopReport,
    SubmissionError,
};
pub use qubit_executor::task::spi::{
    TaskResultHandle,
    TrackedTaskHandle,
};
pub use qubit_executor::{
    CancelResult,
    TaskExecutionError,
    TaskHandle,
    TaskResult,
    TaskStatus,
    TrackedTask,
    TryGet,
};
pub use thread_pool_hooks::ThreadPoolHooks;
pub use thread_pool_stats::ThreadPoolStats;
