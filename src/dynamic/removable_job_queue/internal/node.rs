// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Arc;

use crate::pool_job::QueuedJob;

/// One indexed FIFO slot that owns a linked job entry while it is active.
#[derive(Default)]
pub(in crate::dynamic::removable_job_queue) struct Node {
    /// Job identity held by this live slot, or `None` when the slot is free.
    pub(in crate::dynamic::removable_job_queue) entry: Option<Arc<QueuedJob>>,
    /// Previous live slot in FIFO order.
    pub(in crate::dynamic::removable_job_queue) previous: Option<usize>,
    /// Next live slot in FIFO order.
    pub(in crate::dynamic::removable_job_queue) next: Option<usize>,
}
