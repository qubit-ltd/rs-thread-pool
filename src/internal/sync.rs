// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Atomic primitives selected explicitly for Loom model checking.

#[cfg(not(all(loom, feature = "loom-model")))]
pub(crate) use std::sync::atomic::AtomicUsize;
#[cfg(not(all(loom, feature = "loom-model")))]
pub(crate) use std::sync::atomic::Ordering;

#[cfg(all(loom, feature = "loom-model"))]
pub(crate) use loom::sync::atomic::AtomicUsize;
#[cfg(all(loom, feature = "loom-model"))]
pub(crate) use loom::sync::atomic::Ordering;
