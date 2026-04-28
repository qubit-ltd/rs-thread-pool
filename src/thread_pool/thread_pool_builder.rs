/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
use std::{
    sync::Arc,
    thread,
    time::Duration,
};

use super::thread_pool::ThreadPool;
use super::thread_pool_build_error::ThreadPoolBuildError;
use super::thread_pool_config::ThreadPoolConfig;
use super::thread_pool_inner::ThreadPoolInner;

/// Default thread name prefix used by [`ThreadPoolBuilder`].
const DEFAULT_THREAD_NAME_PREFIX: &str = "qubit-thread-pool";

/// Default idle lifetime for workers above the core pool size.
const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(60);

/// Builder for [`ThreadPool`].
///
/// The default builder uses the available CPU parallelism as both core and
/// maximum pool size, with an unbounded FIFO queue.
///
/// # Author
///
/// Haixing Hu
#[derive(Debug, Clone)]
pub struct ThreadPoolBuilder {
    /// Configured core pool size.
    core_pool_size: usize,
    /// Configured maximum pool size.
    maximum_pool_size: usize,
    /// Optional maximum number of jobs that may wait in the queue.
    queue_capacity: Option<usize>,
    /// Prefix used when naming worker threads.
    thread_name_prefix: String,
    /// Optional stack size in bytes for worker threads.
    stack_size: Option<usize>,
    /// Idle timeout for workers allowed to retire.
    keep_alive: Duration,
    /// Whether core workers may retire after the keep-alive timeout.
    allow_core_thread_timeout: bool,
    /// Whether [`Self::build`] should start all core workers eagerly.
    prestart_core_threads: bool,
}

impl ThreadPoolBuilder {
    /// Sets both the core and maximum pool size to the same value.
    ///
    /// # Parameters
    ///
    /// * `pool_size` - Pool size applied as both core and maximum limits.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn pool_size(mut self, pool_size: usize) -> Self {
        self.core_pool_size = pool_size;
        self.maximum_pool_size = pool_size;
        self
    }

    /// Sets the core pool size.
    ///
    /// A submitted task creates a new worker while the live worker count is
    /// below this value. Once the core size is reached, tasks are queued before
    /// the pool considers growing toward the maximum size.
    ///
    /// # Parameters
    ///
    /// * `core_pool_size` - Core pool size.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn core_pool_size(mut self, core_pool_size: usize) -> Self {
        self.core_pool_size = core_pool_size;
        self
    }

    /// Sets the maximum pool size.
    ///
    /// The pool grows above the core size only when the queue cannot accept a
    /// submitted task.
    ///
    /// # Parameters
    ///
    /// * `maximum_pool_size` - Maximum pool size.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn maximum_pool_size(mut self, maximum_pool_size: usize) -> Self {
        self.maximum_pool_size = maximum_pool_size;
        self
    }

    /// Sets a bounded queue capacity.
    ///
    /// The capacity counts only tasks waiting in the queue. Tasks already held
    /// by worker threads are not included.
    ///
    /// # Parameters
    ///
    /// * `capacity` - Maximum number of queued tasks.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn queue_capacity(mut self, capacity: usize) -> Self {
        self.queue_capacity = Some(capacity);
        self
    }

    /// Uses an unbounded queue.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn unbounded_queue(mut self) -> Self {
        self.queue_capacity = None;
        self
    }

    /// Sets the worker thread name prefix.
    ///
    /// Worker names are created by appending the worker index to this prefix.
    ///
    /// # Parameters
    ///
    /// * `prefix` - Prefix for worker thread names.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn thread_name_prefix(mut self, prefix: &str) -> Self {
        self.thread_name_prefix = prefix.to_owned();
        self
    }

    /// Sets the worker thread stack size.
    ///
    /// # Parameters
    ///
    /// * `stack_size` - Stack size in bytes for each worker thread.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn stack_size(mut self, stack_size: usize) -> Self {
        self.stack_size = Some(stack_size);
        self
    }

    /// Sets the idle timeout for workers above the core pool size.
    ///
    /// # Parameters
    ///
    /// * `keep_alive` - Duration an excess worker may stay idle.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn keep_alive(mut self, keep_alive: Duration) -> Self {
        self.keep_alive = keep_alive;
        self
    }

    /// Allows core workers to retire after the keep-alive timeout.
    ///
    /// # Parameters
    ///
    /// * `allow` - Whether idle core workers may time out.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn allow_core_thread_timeout(mut self, allow: bool) -> Self {
        self.allow_core_thread_timeout = allow;
        self
    }

    /// Starts all core workers during [`Self::build`].
    ///
    /// Without this option, workers are created lazily as tasks are submitted,
    /// matching the default JDK `ThreadPoolExecutor` behavior.
    ///
    /// # Returns
    ///
    /// This builder for fluent configuration.
    #[inline]
    pub fn prestart_core_threads(mut self) -> Self {
        self.prestart_core_threads = true;
        self
    }

    /// Builds the configured thread pool.
    ///
    /// # Returns
    ///
    /// `Ok(ThreadPool)` if the configuration is valid and all requested
    /// prestarted workers are spawned successfully.
    ///
    /// # Errors
    ///
    /// Returns [`ThreadPoolBuildError`] if the configuration is invalid or a
    /// prestarted worker thread cannot be spawned.
    pub fn build(self) -> Result<ThreadPool, ThreadPoolBuildError> {
        self.validate()?;
        let prestart_core_threads = self.prestart_core_threads;
        let inner = Arc::new(ThreadPoolInner::new(ThreadPoolConfig {
            core_pool_size: self.core_pool_size,
            maximum_pool_size: self.maximum_pool_size,
            queue_capacity: self.queue_capacity,
            thread_name_prefix: self.thread_name_prefix,
            stack_size: self.stack_size,
            keep_alive: self.keep_alive,
            allow_core_thread_timeout: self.allow_core_thread_timeout,
        }));
        if prestart_core_threads {
            inner
                .prestart_all_core_threads()
                .map_err(ThreadPoolBuildError::from_rejected_execution)?;
        }
        Ok(ThreadPool::from_inner(inner))
    }

    /// Validates this builder configuration.
    ///
    /// # Returns
    ///
    /// `Ok(())` when all configured values are internally consistent.
    ///
    /// # Errors
    ///
    /// Returns [`ThreadPoolBuildError`] for zero maximum size, core size larger
    /// than maximum size, zero bounded queue capacity, zero stack size, or zero
    /// keep-alive timeout.
    fn validate(&self) -> Result<(), ThreadPoolBuildError> {
        if self.maximum_pool_size == 0 {
            return Err(ThreadPoolBuildError::ZeroMaximumPoolSize);
        }
        if self.core_pool_size > self.maximum_pool_size {
            return Err(ThreadPoolBuildError::CorePoolSizeExceedsMaximum {
                core_pool_size: self.core_pool_size,
                maximum_pool_size: self.maximum_pool_size,
            });
        }
        if self.queue_capacity == Some(0) {
            return Err(ThreadPoolBuildError::ZeroQueueCapacity);
        }
        if self.stack_size == Some(0) {
            return Err(ThreadPoolBuildError::ZeroStackSize);
        }
        if self.keep_alive.is_zero() {
            return Err(ThreadPoolBuildError::ZeroKeepAlive);
        }
        Ok(())
    }
}

impl Default for ThreadPoolBuilder {
    /// Creates a builder with CPU parallelism defaults.
    ///
    /// # Returns
    ///
    /// A builder configured with CPU parallelism for both core and maximum
    /// sizes, an unbounded queue, and the default keep-alive timeout.
    fn default() -> Self {
        let pool_size = default_pool_size();
        Self {
            core_pool_size: pool_size,
            maximum_pool_size: pool_size,
            queue_capacity: None,
            thread_name_prefix: DEFAULT_THREAD_NAME_PREFIX.to_owned(),
            stack_size: None,
            keep_alive: DEFAULT_KEEP_ALIVE,
            allow_core_thread_timeout: false,
            prestart_core_threads: false,
        }
    }
}

/// Returns the default core and maximum pool size for new builders.
///
/// # Returns
///
/// The available CPU parallelism, or `1` if it cannot be detected.
fn default_pool_size() -> usize {
    thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}
