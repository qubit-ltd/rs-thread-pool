// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::Weak;

use crate::dynamic::thread_pool_inner::ThreadPoolInner;
use crate::pool_job::PoolJob;
use crate::pool_job::QueuedJobState;

/// Shared queue identity; all queue ownership transfers lock queue before
/// entry.
pub(crate) struct QueuedJob {
    /// Job ownership or the acceptance/cancellation transition currently in
    /// progress.
    pub(crate) state: Mutex<QueuedJobState>,
    /// Wakes the caller that reserved cancellation during acceptance.
    pub(crate) changed: Condvar,
    /// Originating pool for prepared jobs; ordinary entries use an empty weak
    /// pointer.
    pool: Weak<ThreadPoolInner>,
}

impl QueuedJob {
    /// Creates a prepared entry associated with its originating pool.
    pub(crate) fn prepared(pool: &Arc<ThreadPoolInner>) -> Self {
        Self {
            state: Mutex::new(QueuedJobState::Prepared),
            changed: Condvar::new(),
            pool: Arc::downgrade(pool),
        }
    }

    /// Wraps an ordinary accepted job in an entry for the dynamic FIFO.
    pub(crate) fn new(job: PoolJob) -> Self {
        Self {
            state: Mutex::new(QueuedJobState::Queued(job)),
            changed: Condvar::new(),
            pool: Weak::new(),
        }
    }

    /// Returns whether this entry belongs to the supplied pool.
    pub(crate) fn belongs_to_pool(&self, pool: &Arc<ThreadPoolInner>) -> bool {
        self.pool.ptr_eq(&Arc::downgrade(pool))
    }

    /// Locks the ownership state, recovering an already poisoned mutex.
    pub(crate) fn lock(&self) -> MutexGuard<'_, QueuedJobState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Takes the queued callbacks once; callers must already hold the queue
    /// lock.
    pub(crate) fn take_queued(&self) -> Option<PoolJob> {
        let mut state = self.lock();
        match std::mem::replace(&mut *state, QueuedJobState::Taken) {
            QueuedJobState::Queued(job) => Some(job),
            previous => {
                *state = previous;
                None
            }
        }
    }
}
