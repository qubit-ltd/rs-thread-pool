/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::sync::atomic::Ordering;

use super::fixed_thread_pool_inner::FixedThreadPoolInner;

/// Submit guard that leaves in-flight accounting on drop.
pub struct FixedSubmitGuard<'a> {
    /// Pool whose in-flight counter was entered.
    pub inner: &'a FixedThreadPoolInner,
}

impl Drop for FixedSubmitGuard<'_> {
    /// Leaves submit accounting and wakes shutdown waiters if needed.
    fn drop(&mut self) {
        let previous = self
            .inner
            .inflight_submissions
            .fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "fixed pool submit counter underflow");
        if previous == 1 && !self.inner.accepting.load(Ordering::Acquire) {
            self.inner.state.notify_all();
        }
    }
}
