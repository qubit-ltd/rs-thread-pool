/*******************************************************************************
 *
 *    Copyright (c) 2025 - 2026.
 *    Haixing Hu, Qubit Co. Ltd.
 *
 *    All rights reserved.
 *
 ******************************************************************************/
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

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

/// CAS gate controlling whether submit calls may enter admission.
pub(super) struct SubmissionAdmission {
    /// `true` while new submissions may attempt admission.
    open: AtomicBool,
}

impl SubmissionAdmission {
    /// Creates an admission gate in open state.
    ///
    /// # Returns
    ///
    /// Admission gate that accepts new submissions.
    pub(super) fn new_open() -> Self {
        Self {
            open: AtomicBool::new(true),
        }
    }

    /// Returns whether admission is currently open.
    ///
    /// # Returns
    ///
    /// `true` when submit calls may still enter admission.
    pub(super) fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    /// Closes admission using a CAS transition.
    ///
    /// # Returns
    ///
    /// `true` when this call closed the gate, or `false` when it was already
    /// closed.
    pub(super) fn close(&self) -> bool {
        self.open
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// Result of trying to enter submit in-flight accounting.
pub(super) enum SubmitEnterOutcome {
    /// Submit call entered successfully and must later call `leave`.
    Entered,

    /// Submit call was rejected because admission closed.
    ///
    /// `became_zero_after_rollback` indicates whether rollback decremented the
    /// in-flight counter to zero; shutdown waiters should be notified in that
    /// case.
    Rejected { became_zero_after_rollback: bool },
}

/// Atomic in-flight submit counter.
///
/// This wrapper encapsulates the admission race pattern:
///
/// 1. Check admission before increment.
/// 2. Increment in-flight count.
/// 3. Re-check admission and roll back when it closed concurrently.
pub(super) struct InflightSubmitCounter {
    /// Number of submit calls currently inside admission path.
    count: AtomicUsize,
}

impl InflightSubmitCounter {
    /// Creates an empty in-flight counter.
    ///
    /// # Returns
    ///
    /// Counter initialized to zero.
    pub(super) fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
        }
    }

    /// Returns current in-flight submit count.
    ///
    /// # Returns
    ///
    /// Number of submit calls currently in progress.
    pub(super) fn load(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    /// Attempts to enter one submit operation.
    ///
    /// # Parameters
    ///
    /// * `admission` - Admission gate used to reject late submit attempts.
    ///
    /// # Returns
    ///
    /// [`SubmitEnterOutcome::Entered`] on success, otherwise
    /// [`SubmitEnterOutcome::Rejected`].
    pub(super) fn try_enter(&self, admission: &SubmissionAdmission) -> SubmitEnterOutcome {
        if !admission.is_open() {
            return SubmitEnterOutcome::Rejected {
                became_zero_after_rollback: false,
            };
        }
        self.count.fetch_add(1, Ordering::AcqRel);
        if admission.is_open() {
            return SubmitEnterOutcome::Entered;
        }

        // Admission closed between the first check and increment. Roll back to
        // keep in-flight accounting consistent for shutdown waiters.
        let previous = self.count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(
            previous > 0,
            "thread pool inflight submit counter underflow"
        );
        SubmitEnterOutcome::Rejected {
            became_zero_after_rollback: previous == 1,
        }
    }

    /// Leaves one in-flight submit operation.
    ///
    /// # Returns
    ///
    /// `true` when this call decremented the counter to zero.
    pub(super) fn leave(&self) -> bool {
        let previous = self.count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(
            previous > 0,
            "thread pool inflight submit counter underflow"
        );
        previous == 1
    }
}
