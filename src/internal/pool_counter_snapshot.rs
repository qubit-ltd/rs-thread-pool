// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
/// A best-effort view of pool submissions and task progress.
///
/// Each counter is loaded independently, so values can reflect adjacent
/// instants while tasks move between states. Cross-field equalities are not
/// synchronization guarantees.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PoolCounterSnapshot {
    /// Submitters that may still publish a job or roll back its reservation.
    pub(crate) inflight_submissions: usize,

    /// Published jobs waiting to be claimed by workers.
    pub(crate) queued_tasks: usize,

    /// Jobs currently held by workers.
    pub(crate) running_tasks: usize,

    /// Claimed queued jobs whose cancellation callbacks have not finished.
    pub(crate) cancelling_tasks: usize,

    /// Jobs accepted by the pool since its accounting was created.
    pub(crate) submitted_tasks: usize,

    /// Worker-held jobs completed since the accounting was created.
    pub(crate) completed_tasks: usize,

    /// Queued jobs whose cancellation callbacks finished since accounting
    /// was created.
    pub(crate) cancelled_tasks: usize,
}
