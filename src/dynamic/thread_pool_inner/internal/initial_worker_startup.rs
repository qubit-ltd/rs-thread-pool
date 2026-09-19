// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::mpsc;

use super::initial_worker_job::InitialWorkerJob;

/// Startup channel for a worker waiting for its initial job.
pub(crate) struct InitialWorkerStartup {
    /// Sender used to transfer the initial job to the worker.
    pub(crate) start_sender: mpsc::SyncSender<InitialWorkerJob>,
}
