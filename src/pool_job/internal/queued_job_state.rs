// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::thread::ThreadId;

use crate::pool_job::PoolJob;

/// States preceding and following the unique transfer of a job's callbacks.
pub(crate) enum QueuedJobState {
    /// Prepared but not yet admitted; ticket cancellation is not valid.
    Prepared,
    /// Acceptance runs outside all locks; one ticket may reserve cancellation.
    Accepting { owner: ThreadId, cancel_requested: bool },
    /// Accepted and visible in the FIFO; contains the sole callback owner.
    Queued(PoolJob),
    /// Worker, stop, or a ticket has taken the queued callbacks.
    Taken,
    /// The submitter is cancelling an accepted job before queue publication.
    Cancelling,
    /// Reserved cancellation and accounting have finished.
    Cancelled,
    /// Acceptance panicked, so no callbacks may run.
    Rejected,
}
