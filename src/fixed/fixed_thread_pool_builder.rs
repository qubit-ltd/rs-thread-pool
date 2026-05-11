/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Builder for [`super::FixedThreadPool`].

use std::thread;

use super::fixed_thread_pool::FixedThreadPool;
use crate::{
    ExecutorServiceBuilderError,
    ThreadPoolHooks,
};

/// Default thread name prefix used by [`FixedThreadPoolBuilder`].
const DEFAULT_FIXED_THREAD_NAME_PREFIX: &str = "qubit-fixed-thread-pool";

/// Builder for [`FixedThreadPool`].
///
/// The fixed pool prestarts exactly `pool_size` workers and never changes that
/// count during runtime.
#[derive(Debug, Clone)]
pub struct FixedThreadPoolBuilder {
    /// Number of workers to prestart.
    pub(crate) pool_size: usize,
    /// Optional maximum queued task count.
    pub(crate) queue_capacity: Option<usize>,
    /// Prefix used for worker thread names.
    pub(crate) thread_name_prefix: String,
    /// Optional worker stack size.
    pub(crate) stack_size: Option<usize>,
    /// Optional worker and task lifecycle hooks.
    pub(crate) hooks: ThreadPoolHooks,
}

impl FixedThreadPoolBuilder {
    /// Creates a builder with CPU parallelism defaults.
    ///
    /// # Returns
    ///
    /// A builder with a fixed worker count equal to available parallelism.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the fixed worker count.
    ///
    /// # Parameters
    ///
    /// * `pool_size` - Number of workers to create.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    pub fn pool_size(mut self, pool_size: usize) -> Self {
        self.pool_size = pool_size;
        self
    }

    /// Sets a bounded queue capacity.
    ///
    /// # Parameters
    ///
    /// * `capacity` - Maximum number of queued tasks.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    pub fn queue_capacity(mut self, capacity: usize) -> Self {
        self.queue_capacity = Some(capacity);
        self
    }

    /// Uses an unbounded queue.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    pub fn unbounded_queue(mut self) -> Self {
        self.queue_capacity = None;
        self
    }

    /// Sets the worker thread name prefix.
    ///
    /// # Parameters
    ///
    /// * `prefix` - Prefix used for worker thread names.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    pub fn thread_name_prefix(mut self, prefix: &str) -> Self {
        self.thread_name_prefix = prefix.to_owned();
        self
    }

    /// Sets the worker stack size.
    ///
    /// # Parameters
    ///
    /// * `stack_size` - Stack size in bytes.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    pub fn stack_size(mut self, stack_size: usize) -> Self {
        self.stack_size = Some(stack_size);
        self
    }

    /// Installs a callback invoked when a worker thread starts.
    ///
    /// # Parameters
    ///
    /// * `hook` - Callback receiving the stable worker index.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    pub fn before_worker_start<F>(mut self, hook: F) -> Self
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        self.hooks = self.hooks.before_worker_start(hook);
        self
    }

    /// Installs a callback invoked before a worker thread exits.
    ///
    /// # Parameters
    ///
    /// * `hook` - Callback receiving the stable worker index.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    pub fn after_worker_stop<F>(mut self, hook: F) -> Self
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        self.hooks = self.hooks.after_worker_stop(hook);
        self
    }

    /// Installs a callback invoked before each job is run.
    ///
    /// # Parameters
    ///
    /// * `hook` - Callback receiving the stable worker index.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    pub fn before_task<F>(mut self, hook: F) -> Self
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        self.hooks = self.hooks.before_task(hook);
        self
    }

    /// Installs a callback invoked after each job is run.
    ///
    /// # Parameters
    ///
    /// * `hook` - Callback receiving the stable worker index.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    pub fn after_task<F>(mut self, hook: F) -> Self
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        self.hooks = self.hooks.after_task(hook);
        self
    }

    /// Builds the configured fixed thread pool.
    ///
    /// # Returns
    ///
    /// A fixed pool with all workers prestarted.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorServiceBuilderError`] when configuration is invalid or a
    /// worker thread cannot be spawned.
    pub fn build(self) -> Result<FixedThreadPool, ExecutorServiceBuilderError> {
        self.validate()?;
        FixedThreadPool::new_with_builder(self)
    }

    /// Validates this builder configuration.
    ///
    /// # Returns
    ///
    /// `Ok(())` when configuration is valid.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorServiceBuilderError`] for zero pool size, zero queue capacity,
    /// or zero stack size.
    fn validate(&self) -> Result<(), ExecutorServiceBuilderError> {
        if self.pool_size == 0 {
            return Err(ExecutorServiceBuilderError::ZeroPoolSize);
        }
        if self.queue_capacity == Some(0) {
            return Err(ExecutorServiceBuilderError::ZeroQueueCapacity);
        }
        if self.stack_size == Some(0) {
            return Err(ExecutorServiceBuilderError::ZeroStackSize);
        }
        Ok(())
    }
}

impl Default for FixedThreadPoolBuilder {
    /// Creates a builder using available CPU parallelism.
    ///
    /// # Returns
    ///
    /// Default fixed-pool builder.
    fn default() -> Self {
        Self {
            pool_size: default_fixed_pool_size(),
            queue_capacity: None,
            thread_name_prefix: DEFAULT_FIXED_THREAD_NAME_PREFIX.to_owned(),
            stack_size: None,
            hooks: ThreadPoolHooks::default(),
        }
    }
}

/// Returns the default fixed worker count.
///
/// # Returns
///
/// Available CPU parallelism, or `1` if it cannot be detected.
fn default_fixed_pool_size() -> usize {
    thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}
