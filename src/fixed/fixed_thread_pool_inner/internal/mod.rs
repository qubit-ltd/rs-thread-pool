// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Internal admission types for [`super::FixedThreadPoolInner`].

mod fixed_submit_guard;

pub(super) use fixed_submit_guard::FixedSubmitGuard;
