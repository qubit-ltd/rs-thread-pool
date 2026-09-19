// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
/// Type-erased task operations used by [`super::super::PoolJob`].
pub(crate) trait PoolTask: Send + 'static {
    /// Marks this task as accepted by an executor service.
    fn accept(&self) -> Result<(), ()>;

    /// Runs this task and publishes its result if it was not cancelled first.
    fn run(self: Box<Self>);

    /// Cancels this task before it starts.
    fn cancel(self: Box<Self>);
}
