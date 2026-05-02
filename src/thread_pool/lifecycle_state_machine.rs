/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026 Haixing Hu.
 *
 *    SPDX-License-Identifier: Apache-2.0
 *
 *    Licensed under the Apache License, Version 2.0.
 *
 ******************************************************************************/
use std::sync::atomic::{AtomicU8, Ordering};

use super::thread_pool_lifecycle::ThreadPoolLifecycle;

/// Numeric tag representing [`ThreadPoolLifecycle::Running`].
const LIFECYCLE_RUNNING: u8 = 0;
/// Numeric tag representing [`ThreadPoolLifecycle::Shutdown`].
const LIFECYCLE_SHUTDOWN: u8 = 1;
/// Numeric tag representing [`ThreadPoolLifecycle::Stopping`].
const LIFECYCLE_STOPPING: u8 = 2;

/// Encodes a lifecycle enum as one atomic byte value.
///
/// # Parameters
///
/// * `lifecycle` - Lifecycle variant to encode.
///
/// # Returns
///
/// Stable byte tag for atomic storage.
fn encode_lifecycle(lifecycle: ThreadPoolLifecycle) -> u8 {
    match lifecycle {
        ThreadPoolLifecycle::Running => LIFECYCLE_RUNNING,
        ThreadPoolLifecycle::Shutdown => LIFECYCLE_SHUTDOWN,
        ThreadPoolLifecycle::Stopping => LIFECYCLE_STOPPING,
    }
}

/// Decodes one atomic byte tag into a lifecycle enum.
///
/// # Parameters
///
/// * `tag` - Atomic lifecycle tag to decode.
///
/// # Returns
///
/// Corresponding [`ThreadPoolLifecycle`] value.
///
/// # Panics
///
/// Panics when `tag` is not a supported lifecycle value.
fn decode_lifecycle(tag: u8) -> ThreadPoolLifecycle {
    match tag {
        LIFECYCLE_RUNNING => ThreadPoolLifecycle::Running,
        LIFECYCLE_SHUTDOWN => ThreadPoolLifecycle::Shutdown,
        LIFECYCLE_STOPPING => ThreadPoolLifecycle::Stopping,
        _ => panic!("invalid thread pool lifecycle tag: {tag}"),
    }
}

/// CAS state machine for thread-pool lifecycle transitions.
///
/// This wrapper centralizes lifecycle transition rules:
///
/// 1. `Running -> Shutdown` for graceful shutdown.
/// 2. `Running|Shutdown -> Stopping` for immediate shutdown.
/// 3. `Stopping` is terminal and never transitions back.
pub(super) struct LifecycleStateMachine {
    /// Current lifecycle tag encoded as one byte.
    state: AtomicU8,
}

impl LifecycleStateMachine {
    /// Creates a lifecycle machine in running state.
    ///
    /// # Returns
    ///
    /// Lifecycle machine initialized to [`ThreadPoolLifecycle::Running`].
    pub(super) fn new_running() -> Self {
        Self {
            state: AtomicU8::new(encode_lifecycle(ThreadPoolLifecycle::Running)),
        }
    }

    /// Loads the current lifecycle.
    ///
    /// # Returns
    ///
    /// Current lifecycle value.
    pub(super) fn load(&self) -> ThreadPoolLifecycle {
        decode_lifecycle(self.state.load(Ordering::Acquire))
    }

    /// Attempts graceful transition `Running -> Shutdown`.
    ///
    /// # Returns
    ///
    /// `true` when this call performed the transition; `false` when lifecycle
    /// was already `Shutdown` or `Stopping`.
    pub(super) fn transition_running_to_shutdown(&self) -> bool {
        self.state
            .compare_exchange(
                LIFECYCLE_RUNNING,
                LIFECYCLE_SHUTDOWN,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Ensures lifecycle is moved to `Stopping`.
    ///
    /// # Returns
    ///
    /// `true` when this call performed a transition from `Running` or
    /// `Shutdown`; `false` when lifecycle was already `Stopping`.
    pub(super) fn transition_to_stopping(&self) -> bool {
        loop {
            let current = self.state.load(Ordering::Acquire);
            if current == LIFECYCLE_STOPPING {
                return false;
            }
            if self
                .state
                .compare_exchange(
                    current,
                    LIFECYCLE_STOPPING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
        }
    }
}
