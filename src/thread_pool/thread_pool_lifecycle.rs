/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
/// Lifecycle state for a thread pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ThreadPoolLifecycle {
    /// The pool accepts new tasks and workers wait for queue items.
    Running,

    /// The pool rejects new tasks but drains queued work.
    Shutdown,

    /// The pool rejects new tasks, cancels queued work, and stops workers.
    Stopping,
}

impl ThreadPoolLifecycle {
    /// Returns whether this lifecycle still accepts new work.
    ///
    /// # Returns
    ///
    /// `true` only for [`Self::Running`].
    pub(super) const fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }

    /// Returns whether this lifecycle represents graceful shutdown.
    ///
    /// # Returns
    ///
    /// `true` only for [`Self::Shutdown`].
    pub(super) const fn is_shutdown(self) -> bool {
        matches!(self, Self::Shutdown)
    }
}
