/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::{
    fmt,
    panic::{
        AssertUnwindSafe,
        catch_unwind,
    },
    sync::Arc,
};

/// Shared callback type used by worker and task hooks.
type HookCallback = Arc<dyn Fn(usize) + Send + Sync + 'static>;

/// Worker and task lifecycle hooks shared by thread-pool implementations.
///
/// Hooks are intentionally observational. They run on worker threads and
/// receive the stable worker index that triggered the event. Panics raised by a
/// hook are caught and ignored so instrumentation cannot kill a worker thread
/// or corrupt executor accounting.
#[derive(Clone, Default)]
pub struct ThreadPoolHooks {
    /// Callback invoked when a worker thread starts.
    before_worker_start: Option<HookCallback>,
    /// Callback invoked before a worker thread exits.
    after_worker_stop: Option<HookCallback>,
    /// Callback invoked immediately before a worker runs a job.
    before_task: Option<HookCallback>,
    /// Callback invoked immediately after a worker runs a job.
    after_task: Option<HookCallback>,
}

impl ThreadPoolHooks {
    /// Creates an empty hook set.
    ///
    /// # Returns
    ///
    /// A hook set with no callbacks installed.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs a callback invoked when a worker thread starts.
    ///
    /// # Parameters
    ///
    /// * `hook` - Callback receiving the stable worker index.
    ///
    /// # Returns
    ///
    /// This hook set for fluent configuration.
    #[inline]
    pub fn before_worker_start<F>(mut self, hook: F) -> Self
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        self.before_worker_start = Some(Arc::new(hook));
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
    /// This hook set for fluent configuration.
    #[inline]
    pub fn after_worker_stop<F>(mut self, hook: F) -> Self
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        self.after_worker_stop = Some(Arc::new(hook));
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
    /// This hook set for fluent configuration.
    #[inline]
    pub fn before_task<F>(mut self, hook: F) -> Self
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        self.before_task = Some(Arc::new(hook));
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
    /// This hook set for fluent configuration.
    #[inline]
    pub fn after_task<F>(mut self, hook: F) -> Self
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        self.after_task = Some(Arc::new(hook));
        self
    }

    /// Runs the worker-start hook if one is installed.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker that started.
    #[inline]
    pub(crate) fn run_before_worker_start(&self, worker_index: usize) {
        Self::run_hook(&self.before_worker_start, worker_index);
    }

    /// Runs the worker-stop hook if one is installed.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker that is stopping.
    #[inline]
    pub(crate) fn run_after_worker_stop(&self, worker_index: usize) {
        Self::run_hook(&self.after_worker_stop, worker_index);
    }

    /// Runs the before-task hook if one is installed.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker that claimed a job.
    #[inline]
    pub(crate) fn run_before_task(&self, worker_index: usize) {
        Self::run_hook(&self.before_task, worker_index);
    }

    /// Runs the after-task hook if one is installed.
    ///
    /// # Parameters
    ///
    /// * `worker_index` - Stable index of the worker that completed a job.
    #[inline]
    pub(crate) fn run_after_task(&self, worker_index: usize) {
        Self::run_hook(&self.after_task, worker_index);
    }

    /// Runs one hook callback while isolating hook panics.
    ///
    /// # Parameters
    ///
    /// * `hook` - Optional callback to invoke.
    /// * `worker_index` - Stable worker index passed to the callback.
    #[inline]
    fn run_hook(hook: &Option<HookCallback>, worker_index: usize) {
        if let Some(hook) = hook {
            let _ = catch_unwind(AssertUnwindSafe(|| hook(worker_index)));
        }
    }
}

impl fmt::Debug for ThreadPoolHooks {
    /// Formats hook presence without exposing callback internals.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ThreadPoolHooks")
            .field("before_worker_start", &self.before_worker_start.is_some())
            .field("after_worker_stop", &self.after_worker_stop.is_some())
            .field("before_task", &self.before_task.is_some())
            .field("after_task", &self.after_task.is_some())
            .finish()
    }
}
