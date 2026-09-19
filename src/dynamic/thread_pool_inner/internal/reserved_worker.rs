// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
/// Worker reservation created under the pool state lock before thread spawn.
pub(crate) struct ReservedWorker {
    /// Stable worker index assigned in pool state.
    pub(crate) index: usize,
}
