// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::atomic::Ordering;

use super::super::ThreadPoolInner;

/// Submit guard that leaves in-flight accounting on drop.
pub(crate) struct ThreadPoolSubmitGuard<'a> {
    /// Pool whose in-flight counter was entered.
    pub(crate) inner: &'a ThreadPoolInner,
}

impl Drop for ThreadPoolSubmitGuard<'_> {
    /// Leaves submit accounting and wakes waiters if this was the last
    /// submitter.
    fn drop(&mut self) {
        let previous = self.inner.inflight_submissions.fetch_sub(1, Ordering::Release);
        debug_assert!(previous > 0, "thread pool submit counter underflow");
        if previous == 1 && (self.inner.has_submit_waiters() || self.inner.has_idle_waiters()) {
            self.inner.notify_waiters_after_atomic_change();
        }
    }
}
