// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::atomic::Ordering;

use super::super::ThreadPoolInner;
use super::super::ThreadPoolState;

/// Rolls back a live-worker reservation unless spawning commits it.
///
/// This guard borrows the already-locked state, so unwind cleanup never tries
/// to reacquire the monitor.
#[must_use]
pub(crate) struct ReservedWorker<'a> {
    /// Pool that owns this worker reservation.
    pub(super) inner: &'a ThreadPoolInner,
    /// Pool state already locked by the caller.
    pub(super) state: &'a mut ThreadPoolState,
    /// Stable worker index assigned in pool state.
    pub(crate) index: usize,
    /// Whether the worker count has been published.
    committed: bool,
}

impl<'a> ReservedWorker<'a> {
    /// Creates a guard for a worker index after state has been reserved.
    ///
    /// # Parameters
    ///
    /// * `inner` - Pool that owns the worker.
    /// * `state` - Locked state containing the reserved worker count.
    /// * `index` - Stable worker index assigned to the new worker.
    pub(crate) fn new(inner: &'a ThreadPoolInner, state: &'a mut ThreadPoolState, index: usize) -> Self {
        Self {
            inner,
            state,
            index,
            committed: false,
        }
    }

    /// Publishes the spawned worker in the atomic count and commits its state
    /// reservation.
    pub(crate) fn commit_spawn(&mut self) {
        self.inner.live_worker_count.fetch_add(1, Ordering::Release);
        self.committed = true;
    }
}

impl Drop for ReservedWorker<'_> {
    /// Rolls back the worker reservation unless spawning committed it.
    fn drop(&mut self) {
        if !self.committed {
            self.state.live_workers = self
                .state
                .live_workers
                .checked_sub(1)
                .expect("thread pool live worker counter underflow");
            self.inner.notify_if_idle_or_terminated(self.state);
        }
    }
}
