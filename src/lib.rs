/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
//! # Qubit Thread Pool
//!
//! Dynamic and fixed-size thread pool executor services.
//!
//! # Author
//!
//! Haixing Hu

mod fixed_thread_pool;
mod thread_pool;
mod worker_queue;

pub use fixed_thread_pool::{
    FixedThreadPool,
    FixedThreadPoolBuilder,
};
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
