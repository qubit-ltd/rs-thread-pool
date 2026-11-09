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
//!
//! Use [`ThreadPool`] when workers should grow on demand, or
//! [`FixedThreadPool`] when a fixed set of workers should be started up front.
//! Submission and lifecycle methods are provided by
//! [`ExecutorService`](qubit_executor::service::ExecutorService), which must be
//! imported to call them. The `async-wait` feature adds asynchronous
//! termination waiting and dynamic-pool capacity notifications; it does not
//! schedule futures. Queue capacity limits ordinary waiting-slot admission,
//! not the total number of unfinished tasks.
//!
//! ```
//! use std::io;
//! use qubit_executor::service::ExecutorService;
//! use qubit_thread_pool::ThreadPool;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let pool = ThreadPool::builder()
//!         .core_pool_size(1)
//!         .maximum_pool_size(2)
//!         .queue_capacity(16)
//!         .build()?;
//!     let handle = pool.submit_callable(|| Ok::<usize, io::Error>(42))?;
//!     assert_eq!(handle.get()?, 42);
//!     pool.shutdown();
//!     pool.wait_termination();
//!     Ok(())
//! }
//! ```
//!
//! Further guidance: [English user guide](https://docs.rs/crate/qubit-thread-pool/0.12.1/source/doc/user_guide.md) ·
//! [中文用户指南](https://docs.rs/crate/qubit-thread-pool/0.12.1/source/doc/user_guide.zh_CN.md)

use qubit_executor::service::ExecutorServiceBuilderError;

pub mod dynamic;
pub mod fixed;
mod internal;
mod pool_job;
mod pool_job_submission_error;
mod pool_job_ticket;
#[cfg(test)]
mod tests;
mod thread_pool_hooks;
mod thread_pool_stats;

pub use dynamic::ThreadPool;
pub use dynamic::ThreadPoolBuilder;
pub use fixed::FixedThreadPool;
pub use fixed::FixedThreadPoolBuilder;
pub use pool_job::PoolJob;
pub use pool_job::PoolJobTicket;
pub use pool_job_submission_error::PoolJobSubmissionError;
pub use thread_pool_hooks::ThreadPoolHooks;
pub use thread_pool_stats::ThreadPoolStats;
