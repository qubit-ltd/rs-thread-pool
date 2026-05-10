/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
//! Lifecycle state for fixed-size thread pools.

/// Lifecycle state for a fixed-size thread pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedThreadPoolLifecycle {
    /// The pool accepts new tasks and workers wait for queued work.
    Running,

    /// The pool rejects new tasks but drains queued work.
    Shutdown,

    /// The pool rejects new tasks and cancels queued work.
    Stopping,
}
