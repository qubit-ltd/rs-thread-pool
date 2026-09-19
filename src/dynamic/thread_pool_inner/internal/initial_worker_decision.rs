// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
/// Decision sent to a worker after its initial job is accepted.
#[derive(Clone, Copy)]
pub(crate) enum InitialWorkerDecision {
    /// Run the accepted job.
    Run,
    /// Cancel the accepted job before it starts.
    Cancel,
    /// Abort acceptance without running or cancelling the job.
    Abort,
}
