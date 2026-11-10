// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
//! Atomic primitives selected explicitly for Loom model checking.

#[cfg(not(all(loom, feature = "loom-model")))]
pub(crate) use std::sync::Condvar;
#[cfg(not(all(loom, feature = "loom-model")))]
pub(crate) use std::sync::Mutex;
#[cfg(not(all(loom, feature = "loom-model")))]
pub(crate) use std::sync::MutexGuard;
#[cfg(not(all(loom, feature = "loom-model")))]
pub(crate) use std::sync::atomic::AtomicUsize;
#[cfg(not(all(loom, feature = "loom-model")))]
pub(crate) use std::sync::atomic::Ordering;
#[cfg(not(all(loom, feature = "loom-model")))]
pub(crate) use std::thread::ThreadId;

#[cfg(all(loom, feature = "loom-model"))]
pub(crate) use loom::sync::Condvar;
#[cfg(all(loom, feature = "loom-model"))]
pub(crate) use loom::sync::Mutex;
#[cfg(all(loom, feature = "loom-model"))]
pub(crate) use loom::sync::MutexGuard;
#[cfg(all(loom, feature = "loom-model"))]
pub(crate) use loom::sync::atomic::AtomicUsize;
#[cfg(all(loom, feature = "loom-model"))]
pub(crate) use loom::sync::atomic::Ordering;
#[cfg(all(loom, feature = "loom-model"))]
pub(crate) use loom::thread::ThreadId;

/// Returns the current execution identity from the selected synchronization
/// implementation.
pub(crate) fn current_thread_id() -> ThreadId {
    #[cfg(all(loom, feature = "loom-model"))]
    {
        loom::thread::current().id()
    }
    #[cfg(not(all(loom, feature = "loom-model")))]
    {
        std::thread::current().id()
    }
}
