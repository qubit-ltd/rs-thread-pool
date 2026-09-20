// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
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
        let last = self.inner.admission.leave();
        if last && (!self.inner.admission.is_open() || self.inner.has_idle_waiters()) {
            self.inner.notify_waiters_after_atomic_change();
        }
    }
}
