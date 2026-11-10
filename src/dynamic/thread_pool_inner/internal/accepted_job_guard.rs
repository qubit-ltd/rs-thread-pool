// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Arc;

use super::super::ThreadPoolInner;
use crate::pool_job::QueuedJob;

/// Rolls back an unpublished dynamic queue reservation on unwind.
pub(crate) struct AcceptedJobGuard<'a> {
    /// Pool whose queue reservation is owned by this transaction.
    inner: &'a ThreadPoolInner,
    /// Optional ticket entry whose acceptance state must be rejected on
    /// rollback.
    entry: Option<Arc<QueuedJob>>,
    /// Whether this guard still owns rollback responsibility.
    active: bool,
}

impl<'a> AcceptedJobGuard<'a> {
    /// Tracks a reserved slot and optional entry before user acceptance runs.
    ///
    /// # Parameters
    ///
    /// * `inner` - Pool that owns the reserved queue slot.
    /// * `entry` - Optional ticket entry to reject if the transaction unwinds.
    pub(crate) fn new(inner: &'a ThreadPoolInner, entry: Option<Arc<QueuedJob>>) -> Self {
        Self {
            inner,
            entry,
            active: true,
        }
    }

    /// Commits ownership after the job is published or cancellation completes.
    pub(crate) fn commit(&mut self) {
        self.active = false;
    }
}

impl Drop for AcceptedJobGuard<'_> {
    /// Releases the unpublished slot and wakes any acceptance waiters.
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.inner.release_queue_slot();
        if let Some(entry) = &self.entry {
            crate::pool_job::reject_acceptance(&mut entry.lock());
            entry.changed.notify_all();
        }
        self.inner.notify_waiters_after_atomic_change();
    }
}
