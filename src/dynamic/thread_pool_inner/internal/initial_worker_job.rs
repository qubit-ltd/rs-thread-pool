// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::mpsc;

use super::initial_worker_decision::InitialWorkerDecision;
use crate::PoolJob;

/// Initial job and decision channels exchanged with a newly spawned worker.
pub(crate) struct InitialWorkerJob {
    /// Job whose acceptance callback is run by the newly spawned worker.
    pub(crate) job: PoolJob,
    /// Reports whether the acceptance callback completed.
    pub(crate) acceptance_sender: mpsc::SyncSender<Result<(), ()>>,
    /// Receives the run, cancel, or abort decision.
    pub(crate) decision_receiver: mpsc::Receiver<InitialWorkerDecision>,
}
