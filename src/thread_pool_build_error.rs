/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::io;

use thiserror::Error;

use qubit_executor::service::RejectedExecution;

/// Error returned when a [`crate::ThreadPool`] cannot be built.
///
#[derive(Debug, Error)]
pub enum ThreadPoolBuildError {
    /// The configured maximum pool size is zero.
    #[error("thread pool maximum pool size must be greater than zero")]
    ZeroMaximumPoolSize,

    /// The configured core pool size is greater than the maximum pool size.
    #[error(
        "thread pool core pool size {core_pool_size} exceeds maximum pool size {maximum_pool_size}"
    )]
    CorePoolSizeExceedsMaximum {
        /// Configured core pool size.
        core_pool_size: usize,

        /// Configured maximum pool size.
        maximum_pool_size: usize,
    },

    /// The configured bounded queue capacity is zero.
    #[error("thread pool queue capacity must be greater than zero")]
    ZeroQueueCapacity,

    /// The configured worker stack size is zero.
    #[error("thread pool stack size must be greater than zero")]
    ZeroStackSize,

    /// The configured keep-alive timeout is zero.
    #[error("thread pool keep-alive timeout must be greater than zero")]
    ZeroKeepAlive,

    /// A worker thread could not be spawned.
    #[error("failed to spawn thread pool worker {index}: {source}")]
    SpawnWorker {
        /// Index of the worker that failed to spawn.
        index: usize,

        /// I/O error reported by [`std::thread::Builder::spawn`].
        source: io::Error,
    },
}

impl ThreadPoolBuildError {
    /// Converts a runtime worker-spawn rejection into a build error.
    ///
    /// # Parameters
    ///
    /// * `error` - Rejection produced while prestarting workers during build.
    ///
    /// # Returns
    ///
    /// A build error carrying equivalent failure context.
    pub(crate) fn from_rejected_execution(error: RejectedExecution) -> Self {
        match error {
            RejectedExecution::WorkerSpawnFailed { source } => Self::SpawnWorker {
                index: 0,
                source: io::Error::new(source.kind(), source.to_string()),
            },
            RejectedExecution::Shutdown => Self::SpawnWorker {
                index: 0,
                source: io::Error::other("thread pool shut down during prestart"),
            },
            RejectedExecution::Saturated => Self::SpawnWorker {
                index: 0,
                source: io::Error::other("thread pool saturated during prestart"),
            },
        }
    }
}

impl From<RejectedExecution> for ThreadPoolBuildError {
    /// Converts rejected-execution reasons into build-time thread-pool errors.
    ///
    /// # Parameters
    ///
    /// * `error` - Rejection reason observed during thread prestart.
    ///
    /// # Returns
    ///
    /// A build error that preserves equivalent failure context.
    fn from(error: RejectedExecution) -> Self {
        Self::from_rejected_execution(error)
    }
}
