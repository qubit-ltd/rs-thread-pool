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
use crate::internal::sync::Condvar;
use crate::internal::sync::Mutex;
use crate::internal::sync::MutexGuard;
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
    ///
    /// # Parameters
    ///
    /// * `pool` - Pool that owns the prepared job.
    pub(crate) fn prepared(pool: &Arc<ThreadPoolInner>) -> Self {
        Self {
            state: Mutex::new(QueuedJobState::Prepared),
            changed: Condvar::new(),
            pool: Arc::downgrade(pool),
        }
    }

    /// Wraps an ordinary accepted job in an entry for the dynamic FIFO.
    ///
    /// # Parameters
    ///
    /// * `job` - Accepted job to enqueue.
    pub(crate) fn new(job: PoolJob) -> Self {
        Self {
            state: Mutex::new(QueuedJobState::Queued(job)),
            changed: Condvar::new(),
            pool: Weak::new(),
        }
    }

    /// Returns whether this entry belongs to the supplied pool.
    ///
    /// # Parameters
    ///
    /// * `pool` - Pool to compare with this entry.
    pub(crate) fn belongs_to_pool(&self, pool: &Arc<ThreadPoolInner>) -> bool {
        self.pool.ptr_eq(&Arc::downgrade(pool))
    }

    /// Locks the ownership state, recovering an already poisoned mutex.
    pub(crate) fn lock(&self) -> MutexGuard<'_, QueuedJobState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reserves cancellation during acceptance, returning `None` when the
    /// entry is no longer accepting and `Some(false)` for self or duplicate
    /// cancellation.
    ///
    /// # Parameters
    ///
    /// * `caller` - Logical thread identity attempting to reserve cancellation.
    ///
    /// # Returns
    ///
    /// `Some(true)` when cancellation is reserved, `Some(false)` when the
    /// caller cannot reserve it while acceptance is active, and `None` when
    /// acceptance is no longer active.
    pub(crate) fn reserve_acceptance_cancel(&self, caller: crate::internal::sync::ThreadId) -> Option<bool> {
        let mut state = self.lock();
        if !matches!(*state, QueuedJobState::Accepting { .. }) {
            return None;
        }
        if !crate::pool_job::reserve_accepting_cancel(&mut state, caller) {
            return Some(false);
        }
        Some(true)
    }

    /// Waits for a previously reserved acceptance cancellation to finish.
    pub(crate) fn wait_for_acceptance_cancel(&self) -> bool {
        self.wait_for_acceptance_cancel_inner(|| {})
    }

    /// Waits for an acceptance transition to finish, invoking a hook before
    /// each condition-variable wait.
    ///
    /// # Parameters
    ///
    /// * `before_wait` - Hook invoked while the entry state lock is held.
    ///
    /// # Returns
    ///
    /// `true` when the reserved cancellation completed.
    fn wait_for_acceptance_cancel_inner(&self, mut before_wait: impl FnMut()) -> bool {
        let mut state = self.lock();
        while crate::pool_job::acceptance_transition_active(&state) {
            before_wait();
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        crate::pool_job::cancellation_won(&state)
    }

    /// Test-only synchronization witness for proving rollback wakes a waiter
    /// that already reserved cancellation and entered the condition wait.
    ///
    /// # Parameters
    ///
    /// * `before_wait` - Hook invoked while the entry state lock is held.
    ///
    /// # Returns
    ///
    /// `true` when the reserved cancellation completed.
    #[cfg(all(test, loom, feature = "loom-model"))]
    pub(crate) fn wait_for_acceptance_cancel_with(&self, before_wait: impl FnMut()) -> bool {
        self.wait_for_acceptance_cancel_inner(before_wait)
    }

    /// Reserves cancellation during acceptance and waits for the acceptance
    /// owner to publish its terminal result. A duplicate or self-cancellation
    /// returns immediately without waiting.
    ///
    /// # Parameters
    ///
    /// * `caller` - Logical thread identity requesting cancellation.
    ///
    /// # Returns
    ///
    /// Whether cancellation succeeded, or `None` when acceptance already
    /// finished before the ticket could reserve it.
    pub(crate) fn reserve_and_wait_for_acceptance_cancel(
        &self,
        caller: crate::internal::sync::ThreadId,
    ) -> Option<bool> {
        match self.reserve_acceptance_cancel(caller) {
            Some(true) => Some(self.wait_for_acceptance_cancel()),
            result => result,
        }
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

#[cfg(all(test, loom, feature = "loom-model"))]
mod loom_tests {
    use std::sync::Weak;
    use std::sync::atomic::AtomicUsize as StdAtomicUsize;
    use std::sync::atomic::Ordering as StdOrdering;

    use loom::sync::Arc as LoomArc;
    use loom::sync::atomic::AtomicUsize;
    use loom::sync::atomic::Ordering;
    use loom::thread;

    use super::QueuedJob;
    use crate::internal::PoolAccounting;
    use crate::internal::sync;
    use crate::pool_job::PoolJob;
    use crate::pool_job::QueuedJobState;
    use crate::pool_job::acceptance_transition_active;
    use crate::pool_job::begin_acceptance;
    use crate::pool_job::cancellation_won;
    use crate::pool_job::publish_accepted_job;
    use crate::pool_job::reserve_accepting_cancel;

    // Each entry model explores up to 10,000 branches with at most five Loom
    // threads. There is no preemption or permutation cap; the bound still
    // limits schedules before modeling any outer pool lifecycle or FIFO policy.
    fn check_model<F>(scenario: F)
    where
        F: Fn() + Sync + Send + 'static,
    {
        let mut builder = loom::model::Builder::new();
        builder.max_threads = 5;
        builder.max_branches = 10_000;
        builder.max_duration = None;
        builder.max_permutations = None;
        builder.preemption_bound = None;
        builder.checkpoint_file = None;
        builder.checkpoint_interval = 20_000;
        builder.expect_explicit_explore = false;
        builder.location = false;
        builder.log = false;
        builder.check(scenario);
    }

    // The callback counters are witnesses only; they do not affect any Loom
    // synchronization or scheduling decisions.
    fn counted_job(run: LoomArc<AtomicUsize>, cancel: LoomArc<AtomicUsize>) -> PoolJob {
        let run_count = run.clone();
        let cancel_count = cancel.clone();
        PoolJob::new(
            Box::new(move || {
                run_count.fetch_add(1, Ordering::SeqCst);
            }),
            Box::new(move || {
                cancel_count.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    fn prepared_entry() -> QueuedJob {
        QueuedJob {
            state: sync::Mutex::new(QueuedJobState::Prepared),
            changed: sync::Condvar::new(),
            pool: Weak::new(),
        }
    }

    #[derive(Default)]
    struct CallbackState {
        callback_started: bool,
        release_callback: bool,
        accounting_finished: bool,
    }

    struct CallbackGate {
        state: sync::Mutex<CallbackState>,
        changed: sync::Condvar,
    }

    struct ReservationGate {
        reserved: sync::Mutex<bool>,
        changed: sync::Condvar,
    }

    fn gated_cancel_job(
        runs: LoomArc<AtomicUsize>,
        cancels: LoomArc<AtomicUsize>,
        gate: LoomArc<CallbackGate>,
    ) -> PoolJob {
        let cancel_count = cancels.clone();
        PoolJob::new(
            Box::new(move || {
                runs.fetch_add(1, Ordering::SeqCst);
            }),
            Box::new(move || {
                cancel_count.fetch_add(1, Ordering::SeqCst);
                let mut state = gate.state.lock().unwrap();
                state.callback_started = true;
                gate.changed.notify_all();
                while !state.release_callback {
                    state = gate.changed.wait(state).unwrap();
                }
            }),
        )
    }

    #[test]
    fn test_loom_ticket_entry_rejects_prepared_and_rejected_cancellation() {
        check_model(|| {
            let state = sync::Mutex::new(QueuedJobState::Prepared);
            assert!(!reserve_accepting_cancel(
                &mut state.lock().unwrap(),
                sync::current_thread_id()
            ));
            crate::pool_job::reject_acceptance(&mut state.lock().unwrap());
            assert!(!reserve_accepting_cancel(
                &mut state.lock().unwrap(),
                sync::current_thread_id()
            ));
        });
    }

    #[test]
    fn test_loom_ticket_entry_acceptance_reservation_prevents_publication() {
        let accepted_witness = std::sync::Arc::new(StdAtomicUsize::new(0));
        let cancelled_witness = std::sync::Arc::new(StdAtomicUsize::new(0));
        let accepted_runs = accepted_witness.clone();
        let cancelled_runs = cancelled_witness.clone();
        check_model(move || {
            let runs = LoomArc::new(AtomicUsize::new(0));
            let cancels = LoomArc::new(AtomicUsize::new(0));
            let entry = LoomArc::new(prepared_entry());
            begin_acceptance(&mut entry.lock(), sync::current_thread_id());
            let ticket_entry = entry.clone();
            let ticket =
                thread::spawn(move || ticket_entry.reserve_and_wait_for_acceptance_cancel(sync::current_thread_id()));
            let producer_entry = entry.clone();
            let run_count = runs.clone();
            let cancel_count = cancels.clone();
            let producer = thread::spawn(move || {
                let mut state = producer_entry.lock();
                let job = counted_job(run_count, cancel_count);
                match publish_accepted_job(&mut state, job) {
                    Ok(()) => false,
                    Err(job) => {
                        drop(state);
                        job.cancel();
                        crate::pool_job::finish_cancellation(&mut producer_entry.lock());
                        producer_entry.changed.notify_all();
                        true
                    }
                }
            });
            let cancelled = producer.join().unwrap();
            let ticket_result = ticket.join().unwrap();
            if cancelled {
                cancelled_runs.fetch_add(1, StdOrdering::Relaxed);
                assert_ne!(ticket_result, Some(false));
            } else {
                accepted_runs.fetch_add(1, StdOrdering::Relaxed);
                assert_eq!(ticket_result, None);
            }
            if cancelled {
                assert_eq!(runs.load(Ordering::SeqCst), 0);
                assert_eq!(cancels.load(Ordering::SeqCst), 1);
                assert!(cancellation_won(&entry.lock()));
            } else {
                let job = entry.take_queued().expect("published job remains claimable");
                job.run();
                assert_eq!(runs.load(Ordering::SeqCst), 1);
                assert_eq!(cancels.load(Ordering::SeqCst), 0);
            }
            assert!(!acceptance_transition_active(&entry.lock()));
        });
        assert!(
            accepted_witness.load(StdOrdering::Relaxed) > 0,
            "accepted publication branch was explored"
        );
        assert!(
            cancelled_witness.load(StdOrdering::Relaxed) > 0,
            "reserved cancellation branch was explored"
        );
    }

    #[test]
    fn test_loom_duplicate_and_self_ticket_cancellation_do_not_wait() {
        check_model(|| {
            let entry = LoomArc::new(prepared_entry());
            let owner = sync::current_thread_id();
            begin_acceptance(&mut entry.lock(), owner);
            assert_eq!(
                entry.reserve_and_wait_for_acceptance_cancel(sync::current_thread_id()),
                Some(false)
            );
            let first_entry = entry.clone();
            let first = thread::spawn(move || first_entry.reserve_acceptance_cancel(sync::current_thread_id()));
            let second_entry = entry.clone();
            let second = thread::spawn(move || second_entry.reserve_acceptance_cancel(sync::current_thread_id()));
            let results = [first.join().unwrap(), second.join().unwrap()];
            assert_eq!(results.iter().filter(|result| **result == Some(true)).count(), 1);
            assert_eq!(results.iter().filter(|result| **result == Some(false)).count(), 1);
            crate::pool_job::reject_acceptance(&mut entry.lock());
            entry.changed.notify_all();
        });
    }

    #[test]
    fn test_loom_acceptance_rollback_wakes_reserved_ticket() {
        check_model(|| {
            let runs = LoomArc::new(AtomicUsize::new(0));
            let cancels = LoomArc::new(AtomicUsize::new(0));
            let _unaccepted_job = counted_job(runs.clone(), cancels.clone());
            let accounting = PoolAccounting::new(Some(1));
            assert!(accounting.try_reserve_bounded_slot());
            let entry = LoomArc::new(prepared_entry());
            begin_acceptance(&mut entry.lock(), sync::current_thread_id());
            let waiting_entry = entry.clone();
            let reservation_gate = LoomArc::new(ReservationGate {
                reserved: sync::Mutex::new(false),
                changed: sync::Condvar::new(),
            });
            let ticket_gate = reservation_gate.clone();
            let ticket = thread::spawn(move || {
                assert_eq!(
                    waiting_entry.reserve_acceptance_cancel(sync::current_thread_id()),
                    Some(true),
                );
                Some(waiting_entry.wait_for_acceptance_cancel_with(|| {
                    *ticket_gate.reserved.lock().unwrap() = true;
                    ticket_gate.changed.notify_all();
                }))
            });
            let mut reserved = reservation_gate.reserved.lock().unwrap();
            while !*reserved {
                reserved = reservation_gate.changed.wait(reserved).unwrap();
            }
            drop(reserved);
            crate::pool_job::reject_acceptance(&mut entry.lock());
            accounting.rollback_reserved_slot();
            entry.changed.notify_all();
            assert_eq!(ticket.join().unwrap(), Some(false));
            assert!(matches!(*entry.lock(), QueuedJobState::Rejected));
            assert_eq!(runs.load(Ordering::SeqCst), 0);
            assert_eq!(cancels.load(Ordering::SeqCst), 0);
            assert!(
                accounting.try_reserve_bounded_slot(),
                "guard rollback returns the reserved slot"
            );
            accounting.rollback_reserved_slot();
            assert!(accounting.is_idle());
        });
    }

    #[test]
    fn test_loom_worker_stop_and_ticket_transfer_queued_ownership_once() {
        check_model(|| {
            let runs = LoomArc::new(AtomicUsize::new(0));
            let cancels = LoomArc::new(AtomicUsize::new(0));
            let entry = LoomArc::new(QueuedJob::new(counted_job(runs.clone(), cancels.clone())));
            let mut workers = Vec::new();
            for contender_id in 0..3 {
                let contender = entry.clone();
                workers.push(thread::spawn(move || {
                    contender.take_queued().map(|job| {
                        if contender_id == 0 {
                            job.run();
                        } else {
                            job.cancel();
                        }
                        contender_id
                    })
                }));
            }
            let winners = workers
                .into_iter()
                .filter_map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(winners.len(), 1);
            assert_eq!(runs.load(Ordering::SeqCst) + cancels.load(Ordering::SeqCst), 1);
            assert!(matches!(*entry.lock(), QueuedJobState::Taken));
        });
    }

    #[test]
    fn test_loom_ticket_cancel_completion_holds_entry_until_terminal_state() {
        let full_witness = std::sync::Arc::new(StdAtomicUsize::new(0));
        let reusable_witness = std::sync::Arc::new(StdAtomicUsize::new(0));
        let full_runs = full_witness.clone();
        let reusable_runs = reusable_witness.clone();
        check_model(move || {
            let runs = LoomArc::new(AtomicUsize::new(0));
            let cancels = LoomArc::new(AtomicUsize::new(0));
            let entry = LoomArc::new(prepared_entry());
            let accounting = LoomArc::new(PoolAccounting::new(Some(1)));
            assert!(accounting.try_reserve_bounded_slot());
            begin_acceptance(&mut entry.lock(), sync::current_thread_id());
            let ticket_entry = entry.clone();
            assert!(
                thread::spawn(move || {
                    reserve_accepting_cancel(&mut ticket_entry.lock(), sync::current_thread_id())
                })
                .join()
                .unwrap()
            );
            accounting.publish_accepted_job();
            let gate = LoomArc::new(CallbackGate {
                state: sync::Mutex::new(CallbackState::default()),
                changed: sync::Condvar::new(),
            });
            let cancelled_job = publish_accepted_job(
                &mut entry.lock(),
                gated_cancel_job(runs.clone(), cancels.clone(), gate.clone()),
            )
            .expect_err("reserved ticket owns cancellation");
            accounting.begin_cancel_queued_job();
            assert!(acceptance_transition_active(&entry.lock()));
            assert_eq!(runs.load(Ordering::SeqCst), 0);
            let cancel_entry = entry.clone();
            let cancel_accounting = accounting.clone();
            let cancel_gate = gate.clone();
            let cancellation = thread::spawn(move || {
                cancelled_job.cancel();
                cancel_accounting.finish_cancelled_job();
                crate::pool_job::finish_cancellation(&mut cancel_entry.lock());
                cancel_entry.changed.notify_all();
                let mut state = cancel_gate.state.lock().unwrap();
                state.accounting_finished = true;
                cancel_gate.changed.notify_all();
            });
            let competing_accounting = accounting.clone();
            let competing_gate = gate.clone();
            let full_run = full_runs.clone();
            let reusable_run = reusable_runs.clone();
            let competitor = thread::spawn(move || {
                let mut state = competing_gate.state.lock().unwrap();
                while !state.callback_started {
                    state = competing_gate.changed.wait(state).unwrap();
                }
                drop(state);
                assert!(
                    !competing_accounting.try_reserve_bounded_slot(),
                    "callback keeps capacity occupied"
                );
                full_run.fetch_add(1, StdOrdering::Relaxed);
                let mut state = competing_gate.state.lock().unwrap();
                state.release_callback = true;
                competing_gate.changed.notify_all();
                while !state.accounting_finished {
                    state = competing_gate.changed.wait(state).unwrap();
                }
                drop(state);
                assert!(
                    competing_accounting.try_reserve_bounded_slot(),
                    "capacity is reusable after accounting"
                );
                reusable_run.fetch_add(1, StdOrdering::Relaxed);
                competing_accounting.rollback_reserved_slot();
            });
            cancellation.join().unwrap();
            competitor.join().unwrap();
            assert!(cancellation_won(&entry.lock()));
            assert_eq!(runs.load(Ordering::SeqCst), 0);
            assert_eq!(cancels.load(Ordering::SeqCst), 1);
            assert!(accounting.is_idle());
            let snapshot = accounting.snapshot();
            assert_eq!(snapshot.submitted_tasks, 1);
            assert_eq!(snapshot.cancelled_tasks, 1);
            assert_eq!(snapshot.queued_tasks, 0);
        });
        assert!(full_witness.load(StdOrdering::Relaxed) > 0);
        assert!(reusable_witness.load(StdOrdering::Relaxed) > 0);
        eprintln!(
            "capacity witnesses: unavailable during callback={}, reusable after completion={}",
            full_witness.load(StdOrdering::Relaxed),
            reusable_witness.load(StdOrdering::Relaxed),
        );
    }
}
