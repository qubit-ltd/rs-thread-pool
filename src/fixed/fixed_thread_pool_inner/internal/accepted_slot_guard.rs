// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use super::super::FixedThreadPoolInner;

/// Rolls back a fixed-pool queue slot until publication commits.
pub(crate) struct AcceptedSlotGuard<'a> {
    /// Pool whose queue slot is owned by this transaction.
    inner: &'a FixedThreadPoolInner,
    /// Whether the reserved slot still needs rollback.
    active: bool,
}

impl<'a> AcceptedSlotGuard<'a> {
    /// Tracks a previously reserved queue slot.
    ///
    /// # Parameters
    ///
    /// * `inner` - Fixed pool that owns the reserved queue slot.
    pub(crate) fn new(inner: &'a FixedThreadPoolInner) -> Self {
        Self { inner, active: true }
    }

    /// Commits the slot after publication.
    pub(crate) fn commit(&mut self) {
        self.active = false;
    }
}

impl Drop for AcceptedSlotGuard<'_> {
    /// Releases the unpublished slot when the transaction has not committed.
    fn drop(&mut self) {
        if self.active {
            self.inner.release_queue_slot();
        }
    }
}
