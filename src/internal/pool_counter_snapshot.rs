// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
/// Independently loaded counters for best-effort monitoring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PoolCounterSnapshot {
    pub(crate) inflight_submissions: usize,
    pub(crate) queued_tasks: usize,
    pub(crate) running_tasks: usize,
    pub(crate) cancelling_tasks: usize,
    pub(crate) submitted_tasks: usize,
    pub(crate) completed_tasks: usize,
    pub(crate) cancelled_tasks: usize,
}
