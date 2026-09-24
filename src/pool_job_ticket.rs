// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Arc;
use std::sync::Weak;

use crate::dynamic::thread_pool_inner::ThreadPoolInner;
use crate::pool_job::QueuedJob;
use crate::pool_job::QueuedJobState;

/// A cloneable, one-shot cancellation capability for a dynamic pool job.
///
/// Create tickets with [`crate::ThreadPool::prepare_cancellable_job`]. Keeping
/// a ticket after completion retains no callbacks and does not keep the pool
/// alive.
#[derive(Clone)]
pub struct PoolJobTicket {
    /// Shared ownership state, containing callbacks only while queued.
    pub(crate) entry: Arc<QueuedJob>,
    /// Originating pool, held weakly to avoid extending its lifetime.
    pub(crate) pool: Weak<ThreadPoolInner>,
}

impl PoolJobTicket {
    /// Cancels an accepted job that has not been claimed by a worker.
    ///
    /// Returns `true` only for the caller that wins cancellation, after the
    /// callback, captured-value destruction, and pool accounting finish.
    /// Returns `false` before acceptance, after rejection, or when another
    /// caller, worker, or `stop` has taken ownership.
    ///
    /// If acceptance is in progress, reserves cancellation and waits for its
    /// result without holding a pool lock. The submitting thread then runs the
    /// cancellation callback if acceptance succeeds; otherwise this returns
    /// `false`. Calling from the same job's acceptance callback returns `false`
    /// instead of waiting for itself. Cancellation callbacks may run on either
    /// the submitting thread or this caller and must not wait for work on the
    /// same pool (including `stop`, `join`, and `wait_termination`).
    pub fn cancel_queued(&self) -> bool {
        let Some(pool) = self.pool.upgrade() else {
            return false;
        };
        {
            let mut state = self.entry.lock();
            if let QueuedJobState::Accepting {
                owner,
                cancel_requested,
            } = &mut *state
            {
                if *owner == std::thread::current().id() || *cancel_requested {
                    return false;
                }
                *cancel_requested = true;
                while matches!(*state, QueuedJobState::Accepting { .. } | QueuedJobState::Cancelling) {
                    state = self
                        .entry
                        .changed
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                return matches!(*state, QueuedJobState::Cancelled);
            }
            if !matches!(*state, QueuedJobState::Queued(_)) {
                return false;
            }
        }
        pool.cancel_queued_entry(&self.entry)
    }
}
