// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use qubit_executor::service::ExecutorService;
use qubit_executor::service::SubmissionError;

use crate::PoolJobSubmissionError;
use crate::ThreadPool;
use crate::pool_job::PoolJob;
use crate::pool_job::QueuedJobState;

struct AcceptancePanickingPayload;

impl Drop for AcceptancePanickingPayload {
    fn drop(&mut self) {
        panic!("acceptance payload destructor panicked");
    }
}

/// Observes the private reservation boundary to make this acceptance race
/// deterministic, including an idle worker that could otherwise claim
/// first.
#[test]
fn test_ticket_reserved_during_accept_never_publishes_to_worker() {
    for acceptance_panics in [false, true] {
        let pool = Arc::new(
            ThreadPool::builder()
                .pool_size(1)
                .queue_capacity(1)
                .build()
                .expect("bounded pool builds"),
        );
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        let (ran_tx, ran_rx) = mpsc::channel();
        let (job, ticket) = pool.prepare_cancellable_job(
            Box::new(move || {
                started_tx.send(thread::current().id()).expect("accept starts");
                release_rx.recv().expect("accept released");
                if acceptance_panics {
                    std::panic::panic_any(AcceptancePanickingPayload);
                }
            }),
            Box::new(move || ran_tx.send(()).expect("run observed")),
            Box::new(move || cancelled_tx.send(thread::current().id()).expect("cancel observed")),
        );
        let entry = Arc::clone(&ticket.entry);
        let (submitted_tx, submitted_rx) = mpsc::channel();
        let submit_pool = Arc::clone(&pool);
        thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| submit_pool.submit_job(job)));
            submitted_tx.send(result).expect("submit result");
        });
        let submitter = started_rx.recv_timeout(Duration::from_secs(2)).expect("accept starts");
        let (result_tx, result_rx) = mpsc::channel();
        thread::spawn(move || result_tx.send(ticket.cancel_queued()).expect("cancel result"));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(
            *entry.lock(),
            QueuedJobState::Accepting {
                cancel_requested: true,
                ..
            }
        ) {
            assert!(
                Instant::now() < deadline,
                "ticket reserves cancellation before accept returns"
            );
            thread::yield_now();
        }
        assert_eq!(pool.stats().queued_tasks, 0);
        release_tx.send(()).expect("release acceptance");
        assert_eq!(
            result_rx.recv_timeout(Duration::from_secs(2)).expect("cancel finishes"),
            !acceptance_panics
        );
        let submission = submitted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("submit finishes");
        assert!(
            submission.is_ok(),
            "acceptance payload destructor must not unwind submit"
        );
        let submission = submission.expect("submit boundary remains contained");
        if acceptance_panics {
            assert_eq!(submission, Err(PoolJobSubmissionError::AcceptancePanicked));
            assert!(cancelled_rx.try_recv().is_err());
        } else {
            submission.expect("job accepted");
            assert_eq!(cancelled_rx.try_recv().expect("cancelled on submitter"), submitter);
        }
        assert!(
            ran_rx.try_recv().is_err(),
            "reserved job must never be published to idle worker"
        );
        if acceptance_panics {
            let (done_tx, done_rx) = mpsc::channel();
            pool.submit_job(PoolJob::new(
                Box::new(move || done_tx.send(()).expect("slot reuse task runs")),
                Box::new(|| {}),
            ))
            .expect("failed acceptance must release its queue slot");
            done_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("reused slot task completes");
        }
        pool.shutdown();
        assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
        let stats = pool.stats();
        assert_eq!(stats.submitted_tasks, 1);
        assert_eq!(stats.cancelled_tasks, usize::from(!acceptance_panics));
        assert_eq!(stats.queued_tasks, 0);
        assert_eq!(stats.running_tasks, 0);
    }
}

/// Panics while releasing a queued run closure rather than while calling
/// cancel.
struct PanickingRunCapture;

impl Drop for PanickingRunCapture {
    fn drop(&mut self) {
        panic!("reserved cancellation run capture destructor panicked");
    }
}

#[test]
fn test_ticket_reserved_cancellation_contains_capture_drop_panic_and_wakes_waiter() {
    let pool = Arc::new(ThreadPool::new(1).expect("pool builds"));
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (cancelled_tx, cancelled_rx) = mpsc::channel();
    let capture = PanickingRunCapture;
    let (job, ticket) = pool.prepare_cancellable_job(
        Box::new(move || {
            started_tx.send(()).expect("accept starts");
            release_rx.recv().expect("accept released");
        }),
        Box::new(move || drop(capture)),
        Box::new(move || cancelled_tx.send(()).expect("cancel observed")),
    );
    let entry = Arc::clone(&ticket.entry);
    let (submitted_tx, submitted_rx) = mpsc::channel();
    let submit_pool = Arc::clone(&pool);
    thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| submit_pool.submit_job(job)));
        submitted_tx.send(result).expect("submit result");
    });
    started_rx.recv_timeout(Duration::from_secs(2)).expect("accept starts");
    let (result_tx, result_rx) = mpsc::channel();
    thread::spawn(move || result_tx.send(ticket.cancel_queued()).expect("cancel result"));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !matches!(
        *entry.lock(),
        QueuedJobState::Accepting {
            cancel_requested: true,
            ..
        }
    ) {
        assert!(
            Instant::now() < deadline,
            "ticket reserves cancellation before accept returns"
        );
        thread::yield_now();
    }
    release_tx.send(()).expect("release acceptance");
    let submission = submitted_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("submit finishes");
    // Close admission even when the reproduction unwinds in the submitter.
    pool.shutdown();
    assert!(
        submission.is_ok(),
        "capture Drop panic must not escape reserved cancellation"
    );
    submission
        .expect("submitter does not unwind")
        .expect("job was accepted");
    assert!(
        result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("reserved ticket must wake")
    );
    assert!(matches!(*entry.lock(), QueuedJobState::Cancelled));
    cancelled_rx.try_recv().expect("cancellation callback ran once");
    assert!(cancelled_rx.try_recv().is_err());
    assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
    let stats = pool.stats();
    assert_eq!(stats.submitted_tasks, 1);
    assert_eq!(stats.cancelled_tasks, 1);
    assert_eq!(stats.completed_tasks, 0);
    assert_eq!(stats.queued_tasks, 0);
    assert_eq!(stats.running_tasks, 0);
}

#[test]
fn test_ticket_rejection_contains_capture_drop_panic() {
    for rejection in 0..3 {
        for run_capture_panics in [true, false] {
            let pool = ThreadPool::builder()
                .pool_size(1)
                .queue_capacity(1)
                .prestart_core_threads()
                .build()
                .expect("pool builds");
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            if rejection == 2 {
                pool.submit_job(PoolJob::new(
                    Box::new(move || {
                        started_tx.send(()).expect("blocking worker starts");
                        release_rx.recv().expect("blocking worker released");
                    }),
                    Box::new(|| {}),
                ))
                .expect("blocking job accepted");
                started_rx.recv_timeout(Duration::from_secs(2)).expect("worker starts");
                pool.submit_job(PoolJob::new(Box::new(|| {}), Box::new(|| {})))
                    .expect("queue fills");
            }
            let capture = PanickingRunCapture;
            let panicking: Box<dyn FnOnce() + Send> = Box::new(move || drop(capture));
            let ignored: Box<dyn FnOnce() + Send> = Box::new(|| {});
            let (run, cancel) = if run_capture_panics {
                (panicking, ignored)
            } else {
                (ignored, panicking)
            };
            let (job, ticket) =
                pool.prepare_cancellable_job(Box::new(|| panic!("acceptance callback panics")), run, cancel);
            if rejection == 1 {
                pool.shutdown();
            }
            let submission = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pool.submit_job(job)));
            pool.shutdown();
            if rejection == 2 {
                release_tx.send(()).expect("release blocking worker");
            }
            assert!(
                submission.is_ok(),
                "rejected captures must not replace the submission error with unwind"
            );
            let expected = match rejection {
                0 => PoolJobSubmissionError::AcceptancePanicked,
                1 => PoolJobSubmissionError::Rejected(SubmissionError::Shutdown),
                _ => PoolJobSubmissionError::Rejected(SubmissionError::Saturated),
            };
            assert_eq!(submission.expect("submission does not unwind"), Err(expected));
            assert!(!ticket.cancel_queued());
            if rejection == 0 {
                assert!(matches!(*ticket.entry.lock(), QueuedJobState::Rejected));
            } else {
                assert!(matches!(*ticket.entry.lock(), QueuedJobState::Prepared));
            }
            assert!(pool.wait_termination_timeout(Duration::from_secs(2)));
            let stats = pool.stats();
            assert_eq!(stats.submitted_tasks, if rejection == 2 { 2 } else { 0 });
            assert_eq!(stats.cancelled_tasks, 0);
            assert_eq!(stats.completed_tasks, stats.submitted_tasks);
            assert_eq!(stats.queued_tasks, 0);
            assert_eq!(stats.running_tasks, 0);
        }
    }
}

#[test]
fn test_pool_job_run_cancel_and_discard_contain_panicking_payloads() {
    let run_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        PoolJob::new(
            Box::new(|| std::panic::panic_any(AcceptancePanickingPayload)),
            Box::new(|| {}),
        )
        .run();
    }));
    assert!(run_result.is_ok(), "run payload destructor must not escape");

    let cancel_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        PoolJob::new(
            Box::new(|| {}),
            Box::new(|| std::panic::panic_any(AcceptancePanickingPayload)),
        )
        .cancel();
    }));
    assert!(cancel_result.is_ok(), "cancel payload destructor must not escape");

    let run_capture = AcceptancePanickingPayload;
    let discard_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        PoolJob::new(Box::new(move || drop(run_capture)), Box::new(|| {})).discard();
    }));
    assert!(discard_result.is_ok(), "discard capture destructor must not escape");
}
