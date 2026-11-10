// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use crate::internal::sync::ThreadId;
use crate::pool_job::PoolJob;

/// States preceding and following the unique transfer of a job's callbacks.
pub(crate) enum QueuedJobState {
    /// Prepared but not yet admitted; ticket cancellation is not valid.
    Prepared,
    /// Acceptance runs outside all locks; one ticket may reserve cancellation.
    Accepting {
        /// Thread currently running the acceptance callback.
        owner: ThreadId,
        /// Whether a ticket requested cancellation during acceptance.
        cancel_requested: bool,
    },
    /// Accepted and visible in the FIFO; contains the sole callback owner.
    Queued(
        /// Sole owner of callbacks for the accepted job.
        PoolJob,
    ),
    /// Worker, stop, or a ticket has taken the queued callbacks.
    Taken,
    /// The submitter is cancelling an accepted job before queue publication.
    Cancelling,
    /// Reserved cancellation and accounting have finished.
    Cancelled,
    /// Acceptance panicked, so no callbacks may run.
    Rejected,
}

/// Starts acceptance for the ticket entry. Production and Loom use this same
/// transition so the model exercises the entry protocol directly.
///
/// # Parameters
///
/// * `state` - Entry state to update.
/// * `owner` - Logical thread running the acceptance callback.
pub(crate) fn begin_acceptance(state: &mut QueuedJobState, owner: ThreadId) {
    *state = QueuedJobState::Accepting {
        owner,
        cancel_requested: false,
    };
}

/// Reserves cancellation while acceptance is running. Returns false for the
/// accepting thread itself, duplicate reservations, and terminal states.
///
/// # Parameters
///
/// * `state` - Entry state to inspect and update.
/// * `caller` - Logical thread requesting cancellation.
///
/// # Returns
///
/// Whether the caller reserved cancellation.
pub(crate) fn reserve_accepting_cancel(state: &mut QueuedJobState, caller: ThreadId) -> bool {
    if let QueuedJobState::Accepting {
        owner,
        cancel_requested,
    } = state
    {
        if *owner == caller || *cancel_requested {
            return false;
        }
        *cancel_requested = true;
        return true;
    }
    false
}

/// Reports whether a ticket reservation completed successfully.
///
/// # Parameters
///
/// * `state` - Entry state to inspect.
///
/// # Returns
///
/// Whether the entry reached `Cancelled`.
pub(crate) fn cancellation_won(state: &QueuedJobState) -> bool {
    matches!(state, QueuedJobState::Cancelled)
}

/// Reports whether acceptance or its cancellation transaction is still active.
///
/// # Parameters
///
/// * `state` - Entry state to inspect.
///
/// # Returns
///
/// Whether the entry is still accepting or cancelling.
pub(crate) fn acceptance_transition_active(state: &QueuedJobState) -> bool {
    matches!(state, QueuedJobState::Accepting { .. } | QueuedJobState::Cancelling)
}

/// Completes the terminal notification state after callbacks/accounting finish.
///
/// # Parameters
///
/// * `state` - Entry state to update.
pub(crate) fn finish_cancellation(state: &mut QueuedJobState) {
    *state = QueuedJobState::Cancelled;
}

/// Rejects an entry after acceptance rollback.
///
/// # Parameters
///
/// * `state` - Entry state to update.
pub(crate) fn reject_acceptance(state: &mut QueuedJobState) {
    *state = QueuedJobState::Rejected;
}

/// Publishes the accepted job unless a ticket reserved cancellation first.
/// On cancellation, returns the callbacks to the submitter for cancellation.
///
/// # Parameters
///
/// * `state` - Entry state to update.
/// * `job` - Accepted callbacks to publish or return for cancellation.
///
/// # Returns
///
/// `Ok(())` when the job is published, or the job when cancellation was
/// reserved first.
pub(crate) fn publish_accepted_job(state: &mut QueuedJobState, job: PoolJob) -> Result<(), PoolJob> {
    if matches!(
        state,
        QueuedJobState::Accepting {
            cancel_requested: true,
            ..
        }
    ) {
        *state = QueuedJobState::Cancelling;
        Err(job)
    } else {
        *state = QueuedJobState::Queued(job);
        Ok(())
    }
}
